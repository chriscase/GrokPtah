//! Explicit host process-lifetime ownership (#455).
//!
//! # Why this module exists
//!
//! The desktop/service host used to keep its single-instance advisory lock in
//! `AgentHostHandle`, which is `Clone`. Every clone therefore extended the
//! lifetime of the process lock, and the lock was released only when the last
//! clone dropped. Because completed orchestration runs, background scans,
//! subagents and Computer Use operations all capture host clones inside
//! detached Tokio tasks, "the caller dropped its host" was never a shutdown
//! barrier: a run could reach a terminal state, the control server could be
//! stopped and joined, and `.instance.lock` could *still* be held by a task
//! that had not finished its tail. An immediate same-home restart then failed
//! with `EAGAIN`.
//!
//! # The ownership model
//!
//! * [`HostRuntime`] is **not** `Clone`. It is the single owner of the process
//!   lock and of the task supervisor. Production code holds exactly one.
//! * [`crate::AgentHostHandle`] is `Clone` and is a **request handle**. It can
//!   observe lifecycle state but can neither own nor release the process lock.
//! * [`HostLifecycle`] is the shared state both sides see. It holds the lock in
//!   a `Mutex<Option<InstanceLock>>` so releasing is an *explicit action taken
//!   once*, never a reference-count side effect.
//!
//! Shutdown is ordered (see [`HostRuntime::shutdown`]) and idempotent, and a
//! closed lifecycle fails every authority-bearing operation closed, so handles
//! that outlive the runtime cannot mutate durable state.
//!
//! The lock **file** is never deleted; only the advisory OS lock is released.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use futures::FutureExt;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::host::AgentHostHandle;
use crate::instance_lock::InstanceLock;
use crate::mcp_control::ControlServerHandle;

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

struct SupervisedOutcomeGuard {
    operation: String,
    kind: &'static str,
    failures: Arc<Mutex<Vec<String>>>,
    lifecycle_cancel: CancellationToken,
    abort_is_failure: bool,
    completed: bool,
}

impl SupervisedOutcomeGuard {
    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for SupervisedOutcomeGuard {
    fn drop(&mut self) {
        // Panic is recorded by the surrounding catch_unwind with its payload.
        // A non-panicking drop before completion means the future was aborted
        // or discarded, and must independently survive caller-owned handles.
        if !self.completed
            && !std::thread::panicking()
            && !self.lifecycle_cancel.is_cancelled()
            && self.abort_is_failure
        {
            self.failures.lock().push(format!(
                "supervised {} {} was cancelled before completion",
                self.kind, self.operation
            ));
        }
    }
}

/// The runtime that currently owns durable writes for each home, keyed by the
/// home's instance-lock path.
///
/// Legacy ambient modules resolve their paths through `grokptah_home()` rather
/// than through a handle, so they cannot be handed a caller's authority. They
/// mint from this registry instead: the guard always comes from whichever
/// runtime owns that home *now*, and it is serialized against that runtime's
/// seal — so an ambient write can never race the owner or outlive it. A stale
/// handle gets no privilege from this: it is not the registered owner, and its
/// own lifecycle refuses (see [`HostLifecycle::begin_durable_write`]).
static CURRENT_OWNERS: std::sync::LazyLock<
    Mutex<HashMap<PathBuf, std::sync::Weak<HostLifecycle>>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Process locks this process refused to release because it could not prove a
/// release was safe.
///
/// This exists because "retained on uncertainty" was not true without it. The
/// lock used to live only inside the `HostLifecycle` `Arc`: retaining it meant
/// *not taking it out of that object*, so the moment the last `Arc` was
/// destroyed — a consuming `stop_and_wait`, a dropped runtime with no surviving
/// handle — `InstanceLock`'s own `Drop` released the OS lock and a replacement
/// could start beside work this process could not account for.
///
/// A quarantined lock is moved **out** of the runtime and into here, where
/// nothing drops it. It is held until the process exits, which is what the
/// operator contract promises. This is deliberately a leak: the whole point is
/// that no object's destruction can hand the home on (#455).
static QUARANTINED_LOCKS: std::sync::LazyLock<Mutex<Vec<InstanceLock>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// How many process locks this process has quarantined. Test seam and operator
/// signal; a non-zero value means this process will refuse its own homes until
/// it exits.
pub fn quarantined_process_lock_count() -> usize {
    QUARANTINED_LOCKS.lock().len()
}

/// Registry key for a home.
///
/// `RuntimeHome::from_path` canonicalizes, but `grokptah_home()` returns the
/// configured path verbatim. On any home reached through a symlink — macOS
/// `/var` → `/private/var`, a symlinked `$HOME` — those two differ, so a
/// registration keyed on one would never be found by a lookup keyed on the
/// other and every ambient write would fall through to unowned authority.
/// Both sides go through here so they always agree.
fn owner_key(lock_path: &std::path::Path) -> PathBuf {
    let (Some(parent), Some(name)) = (lock_path.parent(), lock_path.file_name()) else {
        return lock_path.to_path_buf();
    };
    match dunce::canonicalize(parent) {
        Ok(canonical) => canonical.join(name),
        // The home may not exist yet; the verbatim path is then already the
        // only key either side can produce.
        Err(_) => lock_path.to_path_buf(),
    }
}

/// The single-instance lock that governs durable writes to `root`.
///
/// A durable root inside a GrokPtah home is governed by **that home's** lock, so
/// one runtime's `.instance.lock` covers every durable surface it owns —
/// orchestration ledger, Computer Run ledger, event journal — and stopping the
/// runtime seals all of them together.
///
/// A root that is *not* inside a home governs itself: its lock lives in the root
/// rather than in its parent. Deriving the home by simply taking the parent is
/// wrong for such a root — it would put the lock in whatever directory happens
/// to contain it (the system temp directory, a user's home, a checkout) and make
/// every unrelated root under that directory contend for one lock, or worse,
/// believe an unrelated lock protects it. Two writers on the same root are still
/// excluded, which is the property the lease actually needs (#455).
///
/// "Inside a home" is decided by evidence, never by shape: either a live runtime
/// is registered for the parent, or the parent *is* the home this process is
/// configured to use.
fn governing_home_lock(root: &std::path::Path) -> PathBuf {
    if let Some(parent) = root.parent() {
        let parent_lock = owner_key(&parent.join(".instance.lock"));
        let parent_is_a_home = registered_owner(&parent_lock).is_some()
            || owner_key(&crate::discover::grokptah_home().join(".instance.lock")) == parent_lock;
        if parent_is_a_home {
            return parent_lock;
        }
    }
    owner_key(&root.join(".instance.lock"))
}

/// The live runtime that owns `home_lock_path`, if any.
fn registered_owner(home_lock_path: &std::path::Path) -> Option<Arc<HostLifecycle>> {
    let mut owners = CURRENT_OWNERS.lock();
    // Opportunistically drop entries whose runtime is fully gone.
    owners.retain(|_, weak| weak.strong_count() > 0);
    owners
        .get(home_lock_path)
        .and_then(std::sync::Weak::upgrade)
}

/// Mint durable-write authority for the process-wide runtime home.
///
/// * A live runtime owns this home → its authority, serialized against its
///   seal, so the write can never race the owner or outlive it.
/// * A runtime owns it but has sealed or closed → `None`. It is handing the
///   home over, and an ambient write now could land beside a replacement's.
/// * No runtime is registered here, but the OS instance lock is held → `None`.
///   Another process owns this home; registry absence is not authority.
/// * No runtime is registered and the lock is free → `None` as well.
///
/// The last case deserves a note: a process with no host runtime has no
/// authority to grant itself, and a lock that is free *now* says nothing about
/// the instant of the write. Production therefore never mints from absence —
/// `None` means "skip this durable write", never "write anyway".
pub(crate) fn current_durable_write(operation: &str) -> Option<DurableWriteGuard> {
    let lock_path = owner_key(&crate::discover::grokptah_home().join(".instance.lock"));
    registered_owner(&lock_path)?
        .begin_durable_write(operation)
        .ok()
}

/// Lifecycle phase of one host process runtime.
///
/// The phase only ever moves forward: `Running` → `Quiescing` → `Closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostPhase {
    /// Accepting new work.
    Running,
    /// Shutdown started: new admissions are refused, in-flight work is being
    /// cancelled and joined. Durable state is still owned by this process.
    Quiescing,
    /// Shutdown finished. Every authority-bearing operation fails closed and
    /// the process lock has been released.
    Closed,
}

impl HostPhase {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Running,
            1 => Self::Quiescing,
            _ => Self::Closed,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Quiescing => 1,
            Self::Closed => 2,
        }
    }

    /// Operator-facing label used in fail-closed error messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Quiescing => "quiescing",
            Self::Closed => "closed",
        }
    }
}

/// Shared lifecycle state for one host process runtime.
///
/// Cloneable request handles hold an `Arc<HostLifecycle>` so they can *observe*
/// the phase and register supervised tasks. Only [`HostRuntime`] ever calls the
/// transition and release methods.
pub(crate) struct HostLifecycle {
    phase: AtomicU8,
    cancel: CancellationToken,
    tasks: TaskTracker,
    /// `Some` until the owner explicitly releases it. Taking the value out is
    /// what releases the advisory OS lock, so release happens exactly once and
    /// never depends on how many handles are alive.
    instance_lock: Mutex<Option<InstanceLock>>,
    /// Whether this process acquired the lock at construction. A host that
    /// never owned the lock must not claim to have released one.
    acquired_process_lock: bool,
    lock_path: PathBuf,
    /// Closes the check-then-spawn race: supervised spawns take the read side
    /// and re-check the phase under it, while quiescing takes the write side
    /// before sealing the tracker. A task can therefore never be registered
    /// after the join barrier has been armed.
    spawn_gate: parking_lot::RwLock<()>,
    /// Durable-write authority for this runtime's home.
    ///
    /// Every durable mutation must hold a [`DurableWriteGuard`], which can only
    /// be minted here. `in_flight` counts the live guards; sealing sets
    /// `writes_sealed` under the same mutex, so a write can never start after
    /// the seal and the seal can wait for the writes already running. The
    /// process lock is released only once this seal holds, which is what makes
    /// two concurrent durable writers on one home impossible (#455).
    in_flight_writes: parking_lot::Mutex<usize>,
    writes_drained: parking_lot::Condvar,
    writes_sealed: std::sync::atomic::AtomicBool,
    /// The one thread allowed to write after the seal: the runtime performing
    /// its own final flush.
    ///
    /// The flush is a write, and it necessarily happens after the seal — that
    /// is the whole point of sealing first. Rather than a global "flush is
    /// open" flag, which would also let a stale handle through for the
    /// duration, the window is scoped to the exact thread doing the flush. No
    /// other caller, stale or live, can be inside it.
    owner_flush_thread: parking_lot::Mutex<Option<std::thread::ThreadId>>,
    /// Set when this home's lock was moved into process-owned quarantine. The
    /// lock is gone from `instance_lock`, but the home is *more* firmly held,
    /// not less — so ownership queries must still answer "held".
    lock_quarantined: AtomicBool,
    /// Control servers removed from the attachment list for ordered shutdown
    /// but not yet proven stopped. This count deliberately survives
    /// cancellation of the shutdown future, so `Drop` cannot mistake a
    /// detached join for a completed one and release the home.
    control_servers_stopping: AtomicUsize,
    /// Panics/cancellations observed inside supervised work. `TaskTracker`
    /// proves completion, not success, so the lifecycle retains the failure
    /// independently of caller-owned `JoinHandle`s.
    supervised_failures: Arc<Mutex<Vec<String>>>,
}

/// Proof that the runtime holding it still owns durable-write authority for its
/// home.
///
/// It cannot be constructed outside [`HostLifecycle`], and every durable
/// mutation in this crate takes one by reference — so "did I remember to check
/// the lifecycle?" is answered by the compiler rather than by review. While a
/// guard is alive the shutdown seal waits for it; once the seal holds, no new
/// guard is ever issued, so a stale handle cannot write to a home a replacement
/// process now owns.
///
/// `#[must_use]` is load-bearing, not decoration. A guard that is produced and
/// immediately dropped —
///
/// ```ignore
/// lease.begin("writing")?;      // refused: the guard dies on this line
/// let _write = lease.begin("writing")?;  // correct: authority is *held*
/// ```
///
/// — degrades this from held authority into a check-only probe, which is the
/// exact TOCTOU the lease exists to remove: the seal could take effect between
/// the answer and the mutation it was supposed to authorize. Under
/// `clippy -D warnings` the unbound form is a compile error, so this bypass
/// class cannot reappear silently at any of the durable-mutation sites (#455).
#[must_use = "durable-write authority must be held across the mutation it authorizes; \
              binding the guard (`let _write = …`) is what makes it a lease rather than a probe"]
pub(crate) struct DurableWriteGuard {
    lifecycle: Arc<HostLifecycle>,
    /// Whether this guard is part of the in-flight count the seal waits on.
    counted: bool,
}

impl DurableWriteGuard {
    /// Authority for crate tests that drive the store modules directly, with
    /// no host runtime in the picture.
    ///
    /// Deliberately **not** reachable from production code: there is no
    /// `unowned` path outside `cfg(test)`, because absence of a registered
    /// owner is never authority to write a home (#455).
    #[cfg(test)]
    pub(crate) fn unowned_for_test() -> Self {
        Self {
            lifecycle: HostLifecycle::build(None, PathBuf::from("/nonexistent/.instance.lock")),
            counted: false,
        }
    }

    /// The owning runtime's own write, at a point where it is the only possible
    /// writer: host construction (before any handle exists) and the shutdown
    /// flush (after the seal holds).
    ///
    /// Deliberately uncounted — at construction there is nothing to wait for,
    /// and at flush time counting would make the seal wait on itself. It can
    /// only be built from a lifecycle reference the runtime itself holds, so a
    /// stale handle can never reach one.
    pub(crate) fn owner_uncounted(lifecycle: &Arc<HostLifecycle>) -> Self {
        Self {
            lifecycle: lifecycle.clone(),
            counted: false,
        }
    }
}

impl Drop for DurableWriteGuard {
    fn drop(&mut self) {
        if !self.counted {
            return;
        }
        let mut in_flight = self.lifecycle.in_flight_writes.lock();
        *in_flight = in_flight.saturating_sub(1);
        if *in_flight == 0 {
            self.lifecycle.writes_drained.notify_all();
        }
    }
}

impl HostLifecycle {
    pub(crate) fn new(instance_lock: Option<InstanceLock>, lock_path: PathBuf) -> Arc<Self> {
        let lifecycle = Self::build(instance_lock, lock_path);
        // Only a runtime that actually acquired the lock may be the ambient
        // owner; a refused host must never authorize writes for this home.
        if lifecycle.acquired_process_lock {
            CURRENT_OWNERS
                .lock()
                .insert(owner_key(&lifecycle.lock_path), Arc::downgrade(&lifecycle));
        }
        lifecycle
    }

    fn build(instance_lock: Option<InstanceLock>, lock_path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            phase: AtomicU8::new(HostPhase::Running.as_u8()),
            cancel: CancellationToken::new(),
            tasks: TaskTracker::new(),
            acquired_process_lock: instance_lock.is_some(),
            instance_lock: Mutex::new(instance_lock),
            lock_path,
            spawn_gate: parking_lot::RwLock::new(()),
            in_flight_writes: parking_lot::Mutex::new(0),
            writes_drained: parking_lot::Condvar::new(),
            writes_sealed: std::sync::atomic::AtomicBool::new(false),
            owner_flush_thread: parking_lot::Mutex::new(None),
            lock_quarantined: AtomicBool::new(false),
            control_servers_stopping: AtomicUsize::new(0),
            supervised_failures: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub(crate) fn phase(&self) -> HostPhase {
        HostPhase::from_u8(self.phase.load(Ordering::Acquire))
    }

    /// True only while new work may still be admitted.
    pub(crate) fn is_open(&self) -> bool {
        self.phase() == HostPhase::Running
    }

    /// Fail-closed guard for every operation that takes or extends process
    /// authority. Quiescing and closed runtimes both refuse.
    pub(crate) fn ensure_open(&self, operation: &str) -> Result<()> {
        let phase = self.phase();
        if phase == HostPhase::Running {
            return Ok(());
        }
        bail!(
            "GrokPtah host runtime is {}; {operation} is refused because this process \
             no longer holds shutdown authority for {}",
            phase.label(),
            self.lock_path.display()
        )
    }

    /// Whether the advisory OS lock is still held right now.
    pub(crate) fn process_lock_held(&self) -> bool {
        self.instance_lock.lock().is_some() || self.lock_quarantined.load(Ordering::Acquire)
    }

    /// Whether this home's lock was moved into process-owned quarantine.
    pub(crate) fn lock_is_quarantined(&self) -> bool {
        self.lock_quarantined.load(Ordering::Acquire)
    }

    /// Move the process lock out of this runtime and into process-owned
    /// quarantine, so destroying this object — or every handle to it — cannot
    /// release the home.
    ///
    /// Called on every path that declares uncertainty. It is the difference
    /// between "we chose not to release" and "nothing can release": the former
    /// lasts only as long as the object, the latter until the process exits.
    fn quarantine_process_lock(&self) -> bool {
        let held = self.instance_lock.lock().take();
        match held {
            Some(lock) => {
                QUARANTINED_LOCKS.lock().push(lock);
                self.lock_quarantined.store(true, Ordering::Release);
                true
            }
            // Already quarantined by an earlier uncertain stop, or never held.
            None => self.lock_quarantined.load(Ordering::Acquire),
        }
    }

    pub(crate) fn lock_path(&self) -> &std::path::Path {
        &self.lock_path
    }

    pub(crate) fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub(crate) fn tasks(&self) -> &TaskTracker {
        &self.tasks
    }

    /// Register a task with the shutdown join barrier.
    ///
    /// Fails closed once the runtime stops accepting work, so a stale handle
    /// cannot start new process-owned work during or after shutdown.
    pub(crate) fn spawn_supervised<F>(
        &self,
        operation: &str,
        future: F,
    ) -> Result<tokio::task::JoinHandle<F::Output>>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.spawn_supervised_with_abort_policy(operation, future, true)
    }

    /// Register a task whose owner deliberately uses `JoinHandle::abort` as
    /// its normal stop protocol, such as a read-only event aggregator.
    pub(crate) fn spawn_supervised_expected_abort<F>(
        &self,
        operation: &str,
        future: F,
    ) -> Result<tokio::task::JoinHandle<F::Output>>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.spawn_supervised_with_abort_policy(operation, future, false)
    }

    fn spawn_supervised_with_abort_policy<F>(
        &self,
        operation: &str,
        future: F,
        abort_is_failure: bool,
    ) -> Result<tokio::task::JoinHandle<F::Output>>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let _admission = self.spawn_gate.read();
        self.ensure_open(operation)?;
        let operation = operation.to_string();
        let failures = self.supervised_failures.clone();
        let lifecycle_cancel = self.cancel.clone();
        let mut outcome = SupervisedOutcomeGuard {
            operation: operation.clone(),
            kind: "task",
            failures: failures.clone(),
            lifecycle_cancel,
            abort_is_failure,
            completed: false,
        };
        Ok(self.tasks.spawn(async move {
            match std::panic::AssertUnwindSafe(future).catch_unwind().await {
                Ok(output) => {
                    outcome.complete();
                    output
                }
                Err(payload) => {
                    failures.lock().push(format!(
                        "supervised task {operation} panicked: {}",
                        panic_message(payload.as_ref())
                    ));
                    std::panic::resume_unwind(payload)
                }
            }
        }))
    }

    /// Register a future with the shutdown join barrier without spawning it.
    ///
    /// The desktop bootstraps its control plane on the Tauri async runtime,
    /// which is not this crate's to spawn on. Tracking the future still puts it
    /// inside the join barrier, so shutdown cannot finish while a bootstrap is
    /// in flight and later attach a live server to a closed runtime (#455).
    pub(crate) fn track_future<F>(
        &self,
        operation: &str,
        future: F,
    ) -> Result<
        tokio_util::task::task_tracker::TrackedFuture<impl std::future::Future<Output = F::Output>>,
    >
    where
        F: std::future::Future,
    {
        let _admission = self.spawn_gate.read();
        self.ensure_open(operation)?;
        let operation = operation.to_string();
        let failures = self.supervised_failures.clone();
        let lifecycle_cancel = self.cancel.clone();
        let mut outcome = SupervisedOutcomeGuard {
            operation: operation.clone(),
            kind: "future",
            failures: failures.clone(),
            lifecycle_cancel,
            abort_is_failure: true,
            completed: false,
        };
        Ok(self.tasks.track_future(async move {
            match std::panic::AssertUnwindSafe(future).catch_unwind().await {
                Ok(output) => {
                    outcome.complete();
                    output
                }
                Err(payload) => {
                    failures.lock().push(format!(
                        "supervised future {operation} panicked: {}",
                        panic_message(payload.as_ref())
                    ));
                    std::panic::resume_unwind(payload)
                }
            }
        }))
    }

    fn supervised_failures(&self) -> Vec<String> {
        self.supervised_failures.lock().clone()
    }

    /// Arm the join barrier. After this returns no further supervised task can
    /// be registered, because every spawner re-checks the phase under the same
    /// gate.
    fn seal_task_admission(&self) {
        let _sealed = self.spawn_gate.write();
        self.tasks.close();
    }

    /// Mint durable-write authority, or fail closed.
    ///
    /// Refused while quiescing or closed, and refused once the durable-write
    /// seal holds — which is the state the process lock is released in.
    pub(crate) fn begin_durable_write(
        self: &Arc<Self>,
        operation: &str,
    ) -> Result<DurableWriteGuard> {
        // One mutex orders begin against seal: a guard is either counted before
        // the seal takes effect, or refused after it.
        // The runtime's own final flush runs after the seal, on one known
        // thread, and is uncounted so it cannot make the seal wait on itself.
        if self.is_owner_flush_thread() {
            return Ok(DurableWriteGuard {
                lifecycle: self.clone(),
                counted: false,
            });
        }
        let mut in_flight = self.in_flight_writes.lock();
        if self.writes_sealed.load(Ordering::Acquire) {
            bail!(
                "GrokPtah host runtime for {} has released durable-write authority; \
                 {operation} is refused so it cannot race the process that owns the home now",
                self.lock_path.display()
            );
        }
        // Quiescing deliberately still permits writes. New *admissions* are
        // refused at `ensure_session_accepts_new_work`, but work already in
        // flight has to be able to finish writing — that is precisely why the
        // seal comes after the join rather than before it. Refusing here would
        // strand every in-flight finalization and make the join time out, which
        // then retains the lock for a shutdown that was in fact orderly.
        //
        // The write-refusing states are sealed and closed, and a stale handle
        // from a previous runtime is always both.
        if self.phase() == HostPhase::Closed {
            bail!(
                "GrokPtah host runtime for {} is closed; {operation} is refused",
                self.lock_path.display()
            );
        }
        *in_flight += 1;
        Ok(DurableWriteGuard {
            lifecycle: self.clone(),
            counted: true,
        })
    }

    /// Whether durable writes are sealed for this runtime.
    pub(crate) fn durable_writes_sealed(&self) -> bool {
        self.writes_sealed.load(Ordering::Acquire)
    }

    fn is_owner_flush_thread(&self) -> bool {
        *self.owner_flush_thread.lock() == Some(std::thread::current().id())
    }

    /// Open the post-seal flush window for the calling thread only.
    fn open_owner_flush(&self) {
        *self.owner_flush_thread.lock() = Some(std::thread::current().id());
    }

    fn close_owner_flush(&self) {
        *self.owner_flush_thread.lock() = None;
    }

    /// Durable writes running right now.
    pub(crate) fn in_flight_durable_writes(&self) -> usize {
        *self.in_flight_writes.lock()
    }

    /// Seal durable-write authority and wait, bounded, for the writes already
    /// running to finish.
    ///
    /// Returns `true` only when no durable write can still be in progress. A
    /// `false` return means the caller must **not** release the process lock:
    /// releasing it would let a replacement process write the same home while a
    /// writer here is still going.
    fn seal_durable_writes(&self, timeout: std::time::Duration) -> bool {
        let mut in_flight = self.in_flight_writes.lock();
        self.writes_sealed.store(true, Ordering::Release);
        if *in_flight == 0 {
            return true;
        }
        let deadline = std::time::Instant::now() + timeout;
        while *in_flight > 0 {
            if self
                .writes_drained
                .wait_until(&mut in_flight, deadline)
                .timed_out()
                && *in_flight > 0
            {
                return false;
            }
        }
        true
    }

    /// `Running` → `Quiescing`. Returns true for the caller that performed the
    /// transition, so repeated shutdown is a no-op rather than a second pass.
    fn begin_quiesce(&self) -> bool {
        self.phase
            .compare_exchange(
                HostPhase::Running.as_u8(),
                HostPhase::Quiescing.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn mark_closed(&self) {
        self.phase
            .fetch_max(HostPhase::Closed.as_u8(), Ordering::AcqRel);
        // The entry deliberately stays: while this closed lifecycle is still
        // alive it must keep *refusing* ambient writes for this home, rather
        // than leaving the home looking unowned. A replacement runtime
        // overwrites the entry when it registers, and the entry is pruned once
        // this lifecycle is fully dropped.
    }

    /// Release the advisory OS lock exactly once. The lock *file* stays on
    /// disk; only the advisory lock is dropped.
    fn release_process_lock(&self) -> bool {
        let held = self.instance_lock.lock().take();
        held.is_some()
    }
}

/// How long ordered shutdown waits for supervised tasks to finish before it
/// reports the join as failed. Bounded, and never retried: an uncooperative
/// task produces one honest report, not a retry-until-lucky loop.
pub const DEFAULT_SHUTDOWN_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long shutdown (and `Drop`) waits for durable writes already in progress
/// to finish before it gives up on releasing the process lock.
pub const DEFAULT_DURABLE_WRITE_SEAL_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// A fallible piece of durable teardown that must run inside ordered shutdown,
/// after every supervised task has joined and before the process lock is
/// released.
///
/// This is the seam the durable audit ledger plugs into (#462 / #469) without
/// this module taking on ledger scope: the hook runs at exactly the point where
/// no other writer can be active, and its failure is reported rather than
/// swallowed.
pub type ShutdownHook = Box<dyn FnOnce() -> anyhow::Result<()> + Send>;

/// What one [`HostRuntime::shutdown`] call actually did.
///
/// This is the shutdown-ordering proof surface. A report is only `clean` when
/// every supervised task joined, durable writes sealed, every flush and hook
/// succeeded, and the process lock was released with its file intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostShutdownReport {
    /// True when a previous call already completed the ordered shutdown.
    pub already_complete: bool,
    /// Control servers whose accept loop was cancelled and joined.
    pub control_servers_stopped: usize,
    /// Control servers/background workers that could not be proven joined.
    pub control_servers_unjoined: usize,
    /// Supervised tasks still tracked at the moment admissions closed.
    pub supervised_tasks_at_quiesce: usize,
    /// Supervised tasks still tracked after the join barrier. Non-zero only
    /// when `join_timed_out` is set.
    pub supervised_tasks_remaining: usize,
    /// True when supervised work did not finish inside the join timeout. The
    /// process lock is then released only if durable writes still sealed.
    pub join_timed_out: bool,
    /// Panics, cancellations, and join failures retained independently of
    /// caller-owned task handles.
    pub join_errors: Vec<String>,
    /// True when no durable write can still be in progress. The process lock is
    /// never released without this.
    pub durable_writes_sealed: bool,
    /// Durable writes still running when the seal gave up.
    pub durable_writes_in_flight: usize,
    /// Stable failure descriptions from the durable flush and from shutdown
    /// hooks. Never empty on a report that also claims a clean release.
    pub flush_errors: Vec<String>,
    /// Shutdown hooks that ran (audit ledger close and friends).
    pub hooks_run: usize,
    /// True when this call is the one that released the advisory OS lock.
    pub process_lock_released: bool,
    /// Advisory lock state after shutdown.
    pub process_lock_held_after: bool,
    /// True when the lock was deliberately **kept** because releasing it could
    /// have allowed a second durable writer on this home. Fail-closed: a
    /// replacement process is refused until this one exits.
    pub process_lock_retained_for_safety: bool,
    /// The lock file must survive shutdown; only the advisory lock is released.
    pub lock_file_present: bool,
    /// Terminal phase (always [`HostPhase::Closed`]).
    pub phase: HostPhase,
}

impl HostShutdownReport {
    /// Whether this shutdown met every guarantee: all supervised work joined,
    /// durable writes sealed, nothing failed to flush, lock released, file kept.
    pub fn is_clean(&self) -> bool {
        !self.join_timed_out
            && self.control_servers_unjoined == 0
            && self.join_errors.is_empty()
            && self.supervised_tasks_remaining == 0
            && self.durable_writes_sealed
            && self.flush_errors.is_empty()
            && !self.process_lock_held_after
            && !self.process_lock_retained_for_safety
            && self.lock_file_present
            && self.phase == HostPhase::Closed
    }

    /// One operator-facing line. Used by the desktop and service on exit so an
    /// unclean shutdown is visible rather than silent.
    pub fn operator_summary(&self) -> String {
        if self.is_clean() {
            return format!(
                "clean: {} task(s) joined, {} hook(s) run, instance lock released (file kept)",
                self.supervised_tasks_at_quiesce, self.hooks_run
            );
        }
        format!(
            "UNCLEAN: joinTimedOut={} serversUnjoined={} tasksRemaining={} writesSealed={} \
             writesInFlight={} lockRetainedForSafety={} joinErrors={:?} errors={:?}",
            self.join_timed_out,
            self.control_servers_unjoined,
            self.supervised_tasks_remaining,
            self.durable_writes_sealed,
            self.durable_writes_in_flight,
            self.process_lock_retained_for_safety,
            self.join_errors,
            self.flush_errors,
        )
    }
}

/// Exclusive ownership of a home that no runtime owns, held for the whole
/// lifetime of the handle that carries it.
///
/// This replaces what used to be a check-only probe. A momentary
/// `instance_lock_is_held` test proves nothing: a `HostRuntime` can acquire the
/// lock between the probe and the write, and the writer would never know. The
/// only sound answer is to *hold* the lock — so an offline maintenance handle
/// takes the same OS lock a host would and keeps it until it is dropped. A
/// replacement host is then refused for as long as the handle can still write,
/// which is exactly the invariant the probe could not provide (#455).
pub(crate) struct OfflineMaintenanceAuthority {
    _lock: InstanceLock,
    home_lock_path: PathBuf,
}

impl OfflineMaintenanceAuthority {
    /// Take exclusive ownership of an unowned home, or fail.
    fn acquire(home_lock_path: &std::path::Path) -> Result<Self> {
        let home = home_lock_path.parent().unwrap_or(home_lock_path);
        let lock = InstanceLock::try_acquire_path(home_lock_path, home)?;
        Ok(Self {
            _lock: lock,
            home_lock_path: home_lock_path.to_path_buf(),
        })
    }

    pub(crate) fn home_lock_path(&self) -> &std::path::Path {
        &self.home_lock_path
    }
}

/// How a durable handle proves it may write.
enum LeaseState {
    /// A live runtime owns this home; its lifecycle decides, and the write is
    /// counted against that runtime's shutdown seal.
    Runtime(std::sync::Weak<HostLifecycle>),
    /// No runtime owns this home, so the handle owns it: the OS lock is held
    /// for the handle's whole lifetime, not merely probed.
    Offline(OfflineMaintenanceAuthority),
    /// Authority could not be established at open. Every write fails closed
    /// with this reason; the handle never retries, because retrying a refusal
    /// is how a writer talks itself into a home someone else owns.
    Denied(String),
}

/// The durable-write authority a **store handle** carries.
///
/// `OrchStore`, `ComputerStore` and the event journal are cloneable handles on
/// shared durable state, and a clone can outlive the runtime that opened it —
/// an unjoined supervisor, a service handle, a `store()` accessor a caller kept.
/// Binding the lease into the handle is what makes those stale effects fail:
/// the check travels with the clone instead of living at a call site someone
/// has to remember.
///
/// Authority is established **once, at open**, and is either a live runtime's
/// lifecycle or a retained OS lock. There is no path that authorizes a write
/// from the *absence* of an owner, and none that authorizes one from a probe
/// whose answer may already be stale.
#[derive(Clone)]
pub(crate) struct WriteLease {
    home_lock_path: PathBuf,
    state: Arc<LeaseState>,
}

impl WriteLease {
    /// A lease for a durable root, resolved against the home that governs it.
    pub(crate) fn for_store_root(root: &std::path::Path) -> Self {
        Self::for_home_lock(&governing_home_lock(root))
    }

    fn for_home_lock(home_lock_path: &std::path::Path) -> Self {
        let state = match registered_owner(home_lock_path) {
            Some(owner) => LeaseState::Runtime(Arc::downgrade(&owner)),
            None => match OfflineMaintenanceAuthority::acquire(home_lock_path) {
                Ok(authority) => LeaseState::Offline(authority),
                Err(error) => LeaseState::Denied(format!(
                    "no GrokPtah runtime owns {} and its single-instance lock could not be \
                     taken for offline maintenance ({error:#})",
                    home_lock_path.display()
                )),
            },
        };
        Self {
            home_lock_path: home_lock_path.to_path_buf(),
            state: Arc::new(state),
        }
    }

    /// A lease bound to a runtime from the start, for handles the runtime opens
    /// itself.
    ///
    /// Returns `None` when the runtime does not own the home this root lives
    /// in (P1 of the third correction packet): a handle must never borrow
    /// authority for a home its binder does not hold, so a foreign root keeps
    /// whatever authority it established at open — its own retained OS lock —
    /// instead of silently inheriting this runtime's.
    pub(crate) fn bound_to(root: &std::path::Path, lifecycle: &Arc<HostLifecycle>) -> Option<Self> {
        // The same rule `for_store_root` uses, so a bind can never disagree
        // with the authority the handle established at open.
        let home_lock_path = governing_home_lock(root);
        if home_lock_path != owner_key(lifecycle.lock_path()) {
            return None;
        }
        Some(Self {
            home_lock_path,
            state: Arc::new(LeaseState::Runtime(Arc::downgrade(lifecycle))),
        })
    }

    /// A lease that authorizes nothing, for a durable surface that has no home
    /// yet (an in-memory event bus with no persist directory).
    pub(crate) fn denied(reason: impl Into<String>) -> Self {
        Self {
            home_lock_path: PathBuf::new(),
            state: Arc::new(LeaseState::Denied(reason.into())),
        }
    }

    /// Whether this handle is bound to a live runtime rather than owning the
    /// home itself.
    pub(crate) fn is_bound(&self) -> bool {
        matches!(self.state.as_ref(), LeaseState::Runtime(_))
    }

    /// Whether this handle holds the home's OS lock itself.
    pub(crate) fn is_offline_owner(&self) -> bool {
        matches!(self.state.as_ref(), LeaseState::Offline(_))
    }

    pub(crate) fn home_lock_path(&self) -> &std::path::Path {
        &self.home_lock_path
    }

    /// Authorize one durable effect through this handle, or fail closed.
    pub(crate) fn begin(&self, operation: &str) -> Result<DurableWriteGuard> {
        match self.state.as_ref() {
            LeaseState::Runtime(bound) => {
                let Some(lifecycle) = bound.upgrade() else {
                    bail!(
                        "{operation} is refused: this durable handle outlived the GrokPtah \
                         runtime that opened {}",
                        self.home_lock_path.display()
                    );
                };
                lifecycle.begin_durable_write(operation)
            }
            // The OS lock is held for this handle's lifetime, so no runtime can
            // be starting underneath this write. Uncounted because there is no
            // lifecycle to seal against — the retained lock itself is the seal.
            //
            // The guard's home comes from the retained authority rather than
            // this handle's copy of the path, so the write is attributed to the
            // lock actually held and the two can never be reported apart.
            LeaseState::Offline(authority) => Ok(DurableWriteGuard {
                lifecycle: HostLifecycle::build(None, authority.home_lock_path().to_path_buf()),
                counted: false,
            }),
            LeaseState::Denied(reason) => bail!("{operation} is refused: {reason}"),
        }
    }
}

/// A token that can *mint* durable-write authority, without holding any.
///
/// Long-running operations that only write at the end — a provider discovery
/// or qualification round-trip, for instance — take this instead of a
/// [`DurableWriteGuard`]. Holding a guard across network I/O would make an
/// ordinary slow request block the shutdown seal, and a blocked seal means the
/// process lock is retained and the next launch is refused. Minting at the
/// write keeps the authority window as short as the write itself.
#[derive(Clone)]
pub(crate) struct WriteAuthority {
    lifecycle: Arc<HostLifecycle>,
}

impl WriteAuthority {
    pub(crate) fn new(lifecycle: Arc<HostLifecycle>) -> Self {
        Self { lifecycle }
    }

    pub(crate) fn begin(&self, operation: &str) -> Result<DurableWriteGuard> {
        self.lifecycle.begin_durable_write(operation)
    }

    #[cfg(test)]
    pub(crate) fn unowned_for_test() -> Self {
        Self {
            lifecycle: HostLifecycle::build(None, PathBuf::from("/nonexistent/.instance.lock")),
        }
    }
}

/// An open durable-write lease, held by a test to model a writer that is
/// genuinely in flight while shutdown runs.
///
/// It is the same authority production writes hold, so a test holding one
/// reproduces the exact hazard: shutdown must not release the process lock
/// while this is alive.
pub struct DurableWriteLease(#[allow(dead_code)] DurableWriteGuard);

impl DurableWriteLease {
    pub(crate) fn new(guard: DurableWriteGuard) -> Self {
        Self(guard)
    }
}

/// Why [`HostRuntime::attach_control_server`] refused a server.
pub struct ControlServerRejected {
    /// The server handed back, still running. The caller must stop it.
    pub server: ControlServerHandle,
    /// Phase that refused the attach.
    pub phase: HostPhase,
}

/// The single, non-cloneable owner of a GrokPtah host process runtime.
///
/// `HostRuntime` derefs to [`AgentHostHandle`], so callers keep using the same
/// request API; `runtime.clone()` yields a *request handle*, never a second
/// owner. That is the type-level statement of the ownership rule: authority is
/// owned once and shared by reference, never by clone.
pub struct HostRuntime {
    handle: AgentHostHandle,
    lifecycle: Arc<HostLifecycle>,
    control_servers: Mutex<Vec<ControlServerHandle>>,
    hooks: Mutex<Vec<(String, ShutdownHook)>>,
    join_timeout: std::time::Duration,
    write_seal_timeout: std::time::Duration,
    /// Serializes concurrent shutdown callers and memoizes the outcome so
    /// repeated stop is idempotent.
    completed: tokio::sync::Mutex<Option<HostShutdownReport>>,
}

impl HostRuntime {
    pub(crate) fn new(handle: AgentHostHandle, lifecycle: Arc<HostLifecycle>) -> Self {
        Self {
            handle,
            lifecycle,
            control_servers: Mutex::new(Vec::new()),
            hooks: Mutex::new(Vec::new()),
            join_timeout: DEFAULT_SHUTDOWN_JOIN_TIMEOUT,
            write_seal_timeout: DEFAULT_DURABLE_WRITE_SEAL_TIMEOUT,
            completed: tokio::sync::Mutex::new(None),
        }
    }

    /// A cloneable request handle. Request handles never own the process lock.
    pub fn handle(&self) -> AgentHostHandle {
        self.handle.clone()
    }

    pub fn phase(&self) -> HostPhase {
        self.lifecycle.phase()
    }

    /// Whether this process still holds the single-instance advisory lock.
    pub fn holds_process_lock(&self) -> bool {
        self.lifecycle.process_lock_held()
    }

    /// Path of the single-instance lock file. It is never deleted.
    pub fn instance_lock_path(&self) -> PathBuf {
        self.lifecycle.lock_path().to_path_buf()
    }

    /// Supervised tasks currently tracked by this runtime.
    pub fn supervised_task_count(&self) -> usize {
        self.lifecycle.tasks().len()
    }

    /// Durable writes running right now.
    pub fn in_flight_durable_writes(&self) -> usize {
        self.lifecycle.in_flight_durable_writes()
    }

    /// Bound on how long ordered shutdown waits for supervised work.
    pub fn set_join_timeout(&mut self, timeout: std::time::Duration) {
        self.join_timeout = timeout;
    }

    /// Bound on how long shutdown and `Drop` wait for in-flight durable writes
    /// before refusing to release the process lock.
    pub fn set_durable_write_seal_timeout(&mut self, timeout: std::time::Duration) {
        self.write_seal_timeout = timeout;
    }

    /// Register durable teardown that must run after every supervised task has
    /// joined and before the process lock is released. Failures are reported,
    /// never swallowed.
    pub fn register_shutdown_hook(
        &self,
        name: impl Into<String>,
        hook: ShutdownHook,
    ) -> anyhow::Result<()> {
        // Registration takes the same gate shutdown seals before draining, and
        // re-checks the phase under it. A hook is therefore either registered
        // before the drain (and runs) or refused after it (and never silently
        // dropped) — there is no window in between (#455).
        let _admission = self.lifecycle.spawn_gate.read();
        self.lifecycle.ensure_open("registering a shutdown hook")?;
        self.hooks.lock().push((name.into(), hook));
        Ok(())
    }

    /// Hand a control server to the runtime so ordered shutdown stops HTTP/SSE
    /// acceptance *before* run tasks are cancelled and joined.
    ///
    /// Refused once shutdown has begun, and the server is handed back rather
    /// than dropped: a bootstrap that finished late must stop the listener it
    /// just created instead of leaving it serving a closed runtime.
    pub fn attach_control_server(
        &self,
        server: ControlServerHandle,
    ) -> Result<(), ControlServerRejected> {
        // The spawn gate is the same barrier ordered shutdown seals before it
        // drains attached servers, so an attach either lands before the drain
        // or is refused — never after it.
        let _admission = self.lifecycle.spawn_gate.read();
        let phase = self.lifecycle.phase();
        if phase != HostPhase::Running {
            return Err(ControlServerRejected { server, phase });
        }
        self.control_servers.lock().push(server);
        Ok(())
    }

    /// The authenticated control plane this runtime owns, if one is attached.
    ///
    /// Returned by value so no caller holds a reference across the drain in
    /// ordered shutdown: once shutdown has begun this reports `None`, and a
    /// command that reads it is refused rather than handed a stopping server.
    pub fn control_plane(
        &self,
    ) -> Option<(Arc<crate::orchestration::OrchestrationService>, String)> {
        if self.lifecycle.phase() != HostPhase::Running {
            return None;
        }
        let servers = self.control_servers.lock();
        let server = servers.last()?;
        if server.token.is_empty() {
            return None;
        }
        Some((server.orchestration_service(), server.token.clone()))
    }

    /// Address of the attached control plane, for operator logging.
    pub fn control_plane_addr(&self) -> Option<std::net::SocketAddr> {
        self.control_servers.lock().last().map(|server| server.addr)
    }

    /// Track a future on the shutdown join barrier without spawning it.
    ///
    /// For embedders that own their executor (the desktop bootstraps its
    /// control plane on the Tauri async runtime): the returned future must
    /// still be driven, but shutdown will not finish while it is in flight.
    pub fn track<F>(
        &self,
        operation: &str,
        future: F,
    ) -> anyhow::Result<
        tokio_util::task::task_tracker::TrackedFuture<impl std::future::Future<Output = F::Output>>,
    >
    where
        F: std::future::Future,
    {
        self.lifecycle.track_future(operation, future)
    }

    /// Ordered, idempotent shutdown.
    ///
    /// 1. Refuse new admissions (`Running` → `Quiescing`).
    /// 2. Stop HTTP/SSE acceptance and join every attached control server.
    /// 3. Cancel in-flight work, then **join** every supervised task, bounded.
    /// 4. Drain the durable writer threads while authority still exists.
    /// 5. Seal durable-write authority and wait for in-flight writes, bounded.
    /// 6. Flush durable state and run shutdown hooks; record every failure.
    /// 7. Mark closed, then release the advisory OS lock exactly once —
    ///    **only** if durable writes are sealed. Keeps the lock file.
    ///
    /// If the seal fails the lock is deliberately retained: refusing a
    /// replacement is the safe outcome, because releasing would permit two
    /// durable writers on one home.
    pub async fn shutdown(&self) -> HostShutdownReport {
        let mut completed = self.completed.lock().await;
        if let Some(report) = completed.as_ref() {
            let mut report = report.clone();
            report.already_complete = true;
            report.process_lock_released = false;
            report.lock_file_present = self.lifecycle.lock_path().exists();
            report.process_lock_held_after = self.lifecycle.process_lock_held();
            return report;
        }

        // 1. Reject new admissions before anything is torn down.
        self.lifecycle.begin_quiesce();

        // 2. Stop accepting HTTP/SSE work and join the serving tasks. Sealing
        //    task admission first closes the late-attach race: any bootstrap
        //    still in flight is inside the join barrier below, and its attach
        //    will be refused.
        self.lifecycle.seal_task_admission();
        let shutdown_deadline = tokio::time::Instant::now() + self.join_timeout;
        let servers: Vec<ControlServerHandle> = self.control_servers.lock().drain(..).collect();
        self.lifecycle
            .control_servers_stopping
            .fetch_add(servers.len(), Ordering::AcqRel);
        let control_servers_seen = servers.len();
        let mut join_errors = Vec::new();
        let mut control_servers_stopped = 0;
        for mut server in servers {
            let remaining =
                shutdown_deadline.saturating_duration_since(tokio::time::Instant::now());
            let result = server.stop_and_wait_bounded(remaining).await;
            join_errors.extend(result.errors);
            if result.fully_stopped {
                control_servers_stopped += 1;
                self.lifecycle
                    .control_servers_stopping
                    .fetch_sub(1, Ordering::AcqRel);
            }
        }
        let control_servers_unjoined = self
            .lifecycle
            .control_servers_stopping
            .load(Ordering::Acquire);
        if control_servers_unjoined > 0
            && !join_errors
                .iter()
                .any(|error| error.contains("control server"))
        {
            join_errors.push(format!(
                "{control_servers_unjoined} control server(s) remain unjoined after an interrupted shutdown"
            ));
        }
        if control_servers_stopped + control_servers_unjoined < control_servers_seen {
            join_errors.push("control server stop accounting became inconsistent".to_string());
        }

        // 3. Cancel every in-flight unit of work, then join the supervised set.
        self.lifecycle.cancel_token().cancel();
        self.handle.cancel_all_activity().await;
        let supervised_tasks_at_quiesce = self.lifecycle.tasks().len();
        let supervised_join_timed_out =
            tokio::time::timeout_at(shutdown_deadline, self.lifecycle.tasks().wait())
                .await
                .is_err();
        let supervised_tasks_remaining = self.lifecycle.tasks().len();
        join_errors.extend(self.lifecycle.supervised_failures());
        let mut join_timed_out = supervised_join_timed_out || control_servers_unjoined > 0;

        // 4. Drain the durable writer threads, *before* the seal.
        //
        //    The event-journal writer runs on its own thread, so it is not the
        //    owner-flush thread and gets no post-seal exemption. Closing it
        //    after the seal therefore refuses its own final metadata write and
        //    reports an unclean shutdown for a journal that was in fact fine —
        //    which is exactly what hosted macOS caught, and what Linux hid by
        //    happening to have drained the queue already. Draining here, while
        //    authority still exists, lets the writer finish its work; a real
        //    failure still lands in `flush_errors` below and still makes the
        //    shutdown unclean.
        let event_bus = self.handle().event_bus();
        event_bus.begin_close_journal_writer();
        let writer_wait = event_bus.clone();
        let writer_remaining =
            shutdown_deadline.saturating_duration_since(tokio::time::Instant::now());
        let writer_join = tokio::task::spawn_blocking(move || {
            writer_wait.close_journal_writer_bounded(writer_remaining)
        });
        let writer_report = match tokio::time::timeout_at(shutdown_deadline, writer_join).await {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => crate::event_bus::JournalWriterStopReport {
                fully_stopped: false,
                errors: vec![format!("durable event-journal join task failed: {error}")],
            },
            Err(_) => crate::event_bus::JournalWriterStopReport {
                fully_stopped: false,
                errors: vec![format!(
                    "durable event-journal writer did not stop within the shared {:?} shutdown deadline",
                    self.join_timeout
                )],
            },
        };
        if !writer_report.fully_stopped {
            join_timed_out = true;
        }
        let writer_drain_errors = writer_report
            .errors
            .into_iter()
            .map(|error| format!("close the durable event journal: {error}"))
            .collect::<Vec<_>>();

        // 5. Seal durable-write authority. After this no handle — stale or
        //    not — can mutate this home again.
        //
        //    The seal can block for up to its timeout, so it runs on the
        //    blocking pool. If that pool is gone — the runtime is itself being
        //    torn down — fall back to sealing inline rather than reporting
        //    "not sealed": a false negative here would retain the lock and
        //    refuse the next launch for a reason that has nothing to do with a
        //    live writer. The seal is idempotent, so running it twice is safe.
        let write_seal_timeout = self.write_seal_timeout;
        let lifecycle = self.lifecycle.clone();
        let durable_writes_sealed = match tokio::task::spawn_blocking(move || {
            lifecycle.seal_durable_writes(write_seal_timeout)
        })
        .await
        {
            Ok(sealed) => sealed,
            Err(_) => self.lifecycle.seal_durable_writes(write_seal_timeout),
        };
        let durable_writes_in_flight = self.lifecycle.in_flight_durable_writes();

        let mut flush_errors = writer_drain_errors;
        if let Some(error) = self.handle().event_bus().last_persistence_error() {
            let error =
                format!("durable event journal degraded after sealing publication: {error}");
            if !flush_errors.contains(&error) {
                flush_errors.push(error);
            }
        }
        if supervised_join_timed_out {
            flush_errors.push(format!(
                "{supervised_tasks_remaining} supervised task(s) did not finish within {:?}",
                self.join_timeout
            ));
        }
        if !durable_writes_sealed {
            flush_errors.push(format!(
                "{durable_writes_in_flight} durable write(s) still in progress after \
                 {write_seal_timeout:?}"
            ));
        }

        // 6. Flush durable state and run teardown hooks — but only under a
        //    seal that actually holds. Writing while another writer is still
        //    live is exactly the corruption this seam exists to prevent, so a
        //    failed seal skips the flush rather than racing it.
        let hooks: Vec<(String, ShutdownHook)> = {
            let _sealed = self.lifecycle.spawn_gate.write();
            self.hooks.lock().drain(..).collect()
        };
        let hooks_run = if durable_writes_sealed {
            hooks.len()
        } else {
            0
        };
        if durable_writes_sealed {
            self.lifecycle.open_owner_flush();
            if let Err(error) = self.handle.stop() {
                flush_errors.push(format!("stopping the agent host failed: {error:#}"));
            }
            flush_errors.extend(self.handle.flush_durable_state());
            for (name, hook) in hooks {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(hook)) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        flush_errors.push(format!("shutdown hook {name} failed: {error:#}"));
                    }
                    Err(payload) => flush_errors.push(format!(
                        "shutdown hook {name} panicked: {}",
                        panic_message(payload.as_ref())
                    )),
                }
            }
            self.lifecycle.close_owner_flush();
        } else {
            flush_errors.push(format!(
                "durable flush and {} shutdown hook(s) were skipped: the durable-write seal \
                 did not hold, so writing now could race a live writer",
                hooks.len()
            ));
        }

        // A stale publisher and the release decision share one lock. It is
        // therefore impossible for a refused publication to land between the
        // final health read and the process-lock handoff.
        let (release_is_safe, process_lock_released) = self
            .handle()
            .event_bus()
            .with_final_publication_barrier(|persistence_error| {
                if let Some(error) = persistence_error {
                    let error = format!(
                        "durable event journal degraded after sealing publication: {error}"
                    );
                    if !flush_errors.contains(&error) {
                        flush_errors.push(error);
                    }
                }

                // 7. Stale handles must fail closed before the lock can be
                // re-acquired.
                self.lifecycle.mark_closed();

                let release_is_safe = durable_writes_sealed
                    && !join_timed_out
                    && control_servers_unjoined == 0
                    && join_errors.is_empty()
                    && flush_errors.is_empty();
                let released = if release_is_safe {
                    self.lifecycle.release_process_lock()
                } else {
                    self.lifecycle.quarantine_process_lock();
                    false
                };
                (release_is_safe, released)
            });
        let process_lock_held_after = self.lifecycle.process_lock_held();

        let report = HostShutdownReport {
            already_complete: false,
            control_servers_stopped,
            control_servers_unjoined,
            supervised_tasks_at_quiesce,
            supervised_tasks_remaining,
            join_timed_out,
            join_errors,
            durable_writes_sealed,
            durable_writes_in_flight,
            flush_errors,
            hooks_run,
            process_lock_released,
            process_lock_held_after,
            process_lock_retained_for_safety: !release_is_safe && process_lock_held_after,
            lock_file_present: self.lifecycle.lock_path().exists(),
            phase: self.lifecycle.phase(),
        };
        if !report.is_clean() {
            eprintln!(
                "[grokptah] host shutdown for {}: {}",
                self.lifecycle.lock_path().display(),
                report.operator_summary()
            );
        }
        *completed = Some(report.clone());
        report
    }
}

impl std::ops::Deref for HostRuntime {
    type Target = AgentHostHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl Drop for HostRuntime {
    /// A runtime dropped without [`HostRuntime::shutdown`] must not create a
    /// split brain.
    ///
    /// `Drop` cannot await, so it cannot join tasks. What it *can* do — and
    /// must — is make concurrent durable writers impossible before the lock
    /// becomes available: it closes the lifecycle, then seals durable-write
    /// authority and waits, bounded, for the writes already running. Because it
    /// cannot prove journal drain, hooks, task outcomes, or server joins, it
    /// always moves the lock into **process-owned quarantine**. Only the
    /// ordered async shutdown path may release it.
    ///
    /// Quarantine, not mere retention, is what makes that promise true: the
    /// lock no longer lives in any object a caller can destroy, so dropping
    /// this runtime and every handle to it cannot hand the home on.
    ///
    /// Callers that need the full join barrier must await `shutdown()`.
    fn drop(&mut self) {
        // An ordered shutdown already ran and reached its terminal state: it
        // either released the lock (clean) or quarantined it (unclean), and in
        // both cases the lock is no longer this object's to decide about.
        if self.lifecycle.phase() == HostPhase::Closed && self.lifecycle.durable_writes_sealed() {
            debug_assert!(
                !self.lifecycle.process_lock_held() || self.lifecycle.lock_is_quarantined(),
                "a closed, sealed runtime must have released or quarantined its lock"
            );
            return;
        }
        self.lifecycle.begin_quiesce();
        let attached_control_servers = self.control_servers.get_mut().len()
            + self
                .lifecycle
                .control_servers_stopping
                .load(Ordering::Acquire);
        for server in self.control_servers.get_mut().drain(..) {
            server.stop();
        }
        self.lifecycle.cancel_token().cancel();
        self.lifecycle.seal_task_admission();
        self.lifecycle.mark_closed();

        let _sealed = self.lifecycle.seal_durable_writes(self.write_seal_timeout);
        let outstanding_after = self.lifecycle.tasks().len();
        // Even a zero-count snapshot cannot prove the ordered teardown duties
        // (journal close, flush, hooks, join outcomes) ran. Implicit Drop is
        // therefore always fail-closed.
        self.lifecycle.quarantine_process_lock();
        eprintln!(
            "[grokptah] host runtime for {} dropped without an ordered shutdown: \
             {} durable write(s) in flight, {outstanding_after} supervised task(s), and \
             {attached_control_servers} unjoined control server(s). Ordered durable teardown \
             was not proven, so the instance lock is RETAINED for the life of this process. \
             Await HostRuntime::shutdown() for a releasable stop.",
            self.lifecycle.lock_path().display(),
            self.lifecycle.in_flight_durable_writes(),
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AgentHost, HostConfig};
    use crate::orchestration::{
        OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
    };
    use std::sync::atomic::AtomicBool;

    fn lifecycle(dir: &std::path::Path) -> Arc<HostLifecycle> {
        let home = crate::discover::RuntimeHome::from_path(dir).expect("runtime home");
        let path = home.instance_lock_path();
        let lock = InstanceLock::try_acquire_at(&home).expect("acquire instance lock");
        HostLifecycle::new(Some(lock), path)
    }

    fn runtime_with_orchestration() -> (tempfile::TempDir, HostRuntime, Arc<OrchestrationService>) {
        let dir = tempfile::tempdir().unwrap();
        let runtime_home = crate::discover::RuntimeHome::from_path(dir.path().join(".grokptah"))
            .expect("runtime home");
        let runtime =
            AgentHost::create_with_runtime_home(HostConfig::default(), runtime_home.clone())
                .expect("host runtime");
        runtime.start().expect("start host");
        let orch = OrchestrationService::new(
            runtime.handle(),
            runtime.event_bus(),
            OrchStore::open(runtime_home.orchestration_root()).expect("orchestration store"),
            OrchestrationConfig {
                bearer_token: "test-token".into(),
                allowlist: WorkspaceAllowlist::new([dir.path().to_path_buf()]),
                max_concurrent_runs: 1,
                bounds: RunBounds::default(),
            },
        );
        (dir, runtime, orch)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_uncooperative_control_tail_is_bounded_and_quarantined() {
        let (_dir, mut runtime, orch) = runtime_with_orchestration();
        runtime.set_join_timeout(std::time::Duration::from_millis(30));
        assert!(runtime
            .attach_control_server(ControlServerHandle::uncooperative_for_test(orch))
            .is_ok());

        let started = tokio::time::Instant::now();
        let report = runtime.shutdown().await;
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(report.control_servers_unjoined, 1);
        assert!(report.join_timed_out);
        assert!(report
            .join_errors
            .iter()
            .any(|error| error.contains("control server task did not stop")));
        assert!(report.process_lock_retained_for_safety);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_shutdown_after_server_drain_cannot_evade_quarantine() {
        let (dir, mut runtime, orch) = runtime_with_orchestration();
        runtime.set_join_timeout(std::time::Duration::from_secs(30));
        assert!(runtime
            .attach_control_server(ControlServerHandle::uncooperative_for_test(orch))
            .is_ok());
        let quarantined_before = quarantined_process_lock_count();

        let mut shutdown = Box::pin(runtime.shutdown());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut shutdown)
                .await
                .is_err()
        );
        drop(shutdown);
        assert_eq!(
            runtime
                .lifecycle
                .control_servers_stopping
                .load(Ordering::Acquire),
            1
        );
        let resumed = runtime.shutdown().await;
        assert_eq!(resumed.control_servers_unjoined, 1);
        assert!(resumed.join_timed_out);
        assert!(resumed.process_lock_retained_for_safety);
        drop(runtime);
        assert_eq!(quarantined_process_lock_count(), quarantined_before + 1);

        let replacement_home =
            crate::discover::RuntimeHome::from_path(dir.path().join(".grokptah"))
                .expect("replacement home");
        assert!(
            AgentHost::create_with_runtime_home(HostConfig::default(), replacement_home).is_err()
        );
    }

    /// The join barrier, isolated from any agent work: a task that has already
    /// published its terminal signal but is still running its tail must be
    /// joined before shutdown reports completion (#455 acceptance criterion:
    /// "no terminal run counts as shutdown-complete while a spawned task still
    /// holds authority"). This is deterministic — the test controls exactly
    /// when the tail finishes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_joins_a_terminal_but_unfinished_task() {
        let dir = tempfile::tempdir().unwrap();
        let lifecycle = lifecycle(dir.path());

        let reached_terminal = Arc::new(tokio::sync::Notify::new());
        let release_tail = Arc::new(tokio::sync::Notify::new());
        let tail_finished = Arc::new(AtomicBool::new(false));

        let terminal_signal = reached_terminal.clone();
        let tail_gate = release_tail.clone();
        let finished = tail_finished.clone();
        lifecycle
            .spawn_supervised("test task", async move {
                // Terminal state is observable here...
                terminal_signal.notify_one();
                // ...but the task still holds authority until its tail ends.
                tail_gate.notified().await;
                finished.store(true, Ordering::Release);
            })
            .expect("running lifecycle accepts supervised work");

        reached_terminal.notified().await;
        assert!(
            !tail_finished.load(Ordering::Acquire),
            "the tail must still be running when shutdown starts"
        );

        // Arm the barrier without releasing the tail, then prove shutdown is
        // still blocked, then release it.
        lifecycle.begin_quiesce();
        lifecycle.seal_task_admission();
        let waiter = lifecycle.tasks().wait();
        tokio::pin!(waiter);
        tokio::select! {
            _ = &mut waiter => panic!("join barrier returned while the task was still running"),
            _ = std::future::ready(()) => {}
        }
        assert!(
            lifecycle.process_lock_held(),
            "lock held until the join ends"
        );

        release_tail.notify_one();
        waiter.await;
        assert!(
            tail_finished.load(Ordering::Acquire),
            "the barrier must not return before the tail completed"
        );

        lifecycle.mark_closed();
        assert!(lifecycle.release_process_lock());
        assert!(!lifecycle.process_lock_held());
        assert!(
            dir.path().join(".instance.lock").is_file(),
            "the lock file is retained; only the advisory lock is released"
        );
    }

    /// Quiescing seals task admission: a stale handle cannot register new
    /// process-owned work after the barrier is armed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quiescing_refuses_new_supervised_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let lifecycle = lifecycle(dir.path());
        assert!(lifecycle.spawn_supervised("first", async {}).is_ok());

        lifecycle.begin_quiesce();
        lifecycle.seal_task_admission();
        let refused = lifecycle.spawn_supervised("second", async {});
        assert!(
            refused.is_err(),
            "quiescing must refuse new supervised work"
        );
        let message = refused.unwrap_err().to_string();
        assert!(message.contains("quiescing"), "{message}");

        lifecycle.tasks().wait().await;
        lifecycle.mark_closed();
        assert!(lifecycle.spawn_supervised("third", async {}).is_err());
        assert!(lifecycle.release_process_lock());
    }

    /// P1 from independent review: work that never finishes must produce one
    /// bounded, operator-visible failure — not a retry loop, and not a silent
    /// claim of a clean stop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_uncooperative_task_produces_a_bounded_unclean_report() {
        let dir = tempfile::tempdir().unwrap();
        let lifecycle = lifecycle(dir.path());
        let started = Arc::new(tokio::sync::Notify::new());
        let started_tx = started.clone();
        lifecycle
            .spawn_supervised("an uncooperative task", async move {
                started_tx.notify_one();
                // Ignores the shutdown token on purpose.
                std::future::pending::<()>().await;
            })
            .expect("running lifecycle accepts work");
        started.notified().await;

        lifecycle.begin_quiesce();
        lifecycle.seal_task_admission();
        let joined = tokio::time::timeout(
            std::time::Duration::from_millis(150),
            lifecycle.tasks().wait(),
        )
        .await;
        assert!(joined.is_err(), "the join must time out, bounded");
        assert_eq!(lifecycle.tasks().len(), 1);

        // A durable seal does not prove an external effect ended. The task is
        // still live, so authority must be quarantined even though no home
        // write is currently in flight.
        assert!(lifecycle.seal_durable_writes(std::time::Duration::from_millis(50)));
        lifecycle.mark_closed();
        assert!(lifecycle.quarantine_process_lock());
        assert!(lifecycle.process_lock_held());
        assert!(lifecycle.lock_is_quarantined());
    }

    /// A durable write in flight is what actually blocks the release: the seal
    /// must refuse, so the caller keeps the lock rather than handing the home
    /// to a replacement while a writer is live.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_running_durable_write_refuses_the_seal() {
        let dir = tempfile::tempdir().unwrap();
        let lifecycle = lifecycle(dir.path());
        let write = lifecycle
            .begin_durable_write("a long durable write")
            .expect("running lifecycle issues authority");
        assert_eq!(lifecycle.in_flight_durable_writes(), 1);

        assert!(
            !lifecycle.seal_durable_writes(std::time::Duration::from_millis(80)),
            "the seal must refuse while a writer is live"
        );
        assert!(lifecycle.durable_writes_sealed());
        // Sealed means no *new* writer, even though the old one is still live.
        assert!(lifecycle.begin_durable_write("a second write").is_err());

        drop(write);
        assert_eq!(lifecycle.in_flight_durable_writes(), 0);
        // Now the seal is satisfiable, and the release is safe.
        assert!(lifecycle.seal_durable_writes(std::time::Duration::from_millis(80)));
        lifecycle.mark_closed();
        assert!(lifecycle.release_process_lock());
        assert!(dir.path().join(".instance.lock").is_file());
    }

    /// The ambient authority used by legacy modules that resolve their paths
    /// through `grokptah_home()` must follow the live owner, never a stale one.
    #[test]
    fn ambient_authority_follows_the_live_owner() {
        let _serial = crate::discover::home_override_serial();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".grokptah");
        std::fs::create_dir_all(&home).unwrap();
        crate::discover::set_grokptah_home_override(Some(home.clone()));

        // No registered owner is never authority: a process with no runtime
        // has none to grant itself (#455).
        assert!(
            current_durable_write("no owner").is_none(),
            "registry absence must never mint authority"
        );

        let first = lifecycle(&home);
        assert!(
            current_durable_write("live owner").is_some(),
            "a running owner authorizes ambient writes"
        );

        // A closed owner that is still alive must keep refusing: it is handing
        // the home over, and an ambient write now could land beside a
        // replacement's.
        first.begin_quiesce();
        assert!(first.seal_durable_writes(std::time::Duration::from_millis(50)));
        first.mark_closed();
        assert!(first.release_process_lock());
        assert!(
            current_durable_write("closed owner").is_none(),
            "a closed owner must not authorize ambient writes"
        );

        // A replacement registers and becomes the authority.
        let second = lifecycle(&home);
        assert!(current_durable_write("replacement owner").is_some());

        // The same home reached through a symlink must resolve to the same
        // owner. Without one canonical identity the registry key and the
        // lookup key diverge — which is exactly how macOS `/var` versus
        // `/private/var` fell through to unowned authority.
        let alias_root = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias_root).unwrap();
        crate::discover::set_grokptah_home_override(Some(alias_root.join(".grokptah")));
        assert!(
            current_durable_write("owner reached through a path alias").is_some(),
            "a symlinked home must resolve to the same owner, not to unowned authority"
        );
        crate::discover::set_grokptah_home_override(Some(home.clone()));
        second.mark_closed();
        second.release_process_lock();
        drop(first);
        drop(second);
        crate::discover::set_grokptah_home_override(None);
    }

    /// The advisory lock is released exactly once, and only by an owner that
    /// actually acquired it.
    #[test]
    fn release_is_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let lifecycle = lifecycle(dir.path());
        assert!(lifecycle.acquired_process_lock);
        assert!(lifecycle.process_lock_held());
        assert!(lifecycle.release_process_lock());
        assert!(!lifecycle.release_process_lock());
        assert!(!lifecycle.process_lock_held());
        assert!(dir.path().join(".instance.lock").is_file());

        let never_owned = HostLifecycle::new(None, dir.path().join(".instance.lock"));
        assert!(!never_owned.acquired_process_lock);
        assert!(!never_owned.release_process_lock());
    }
}
