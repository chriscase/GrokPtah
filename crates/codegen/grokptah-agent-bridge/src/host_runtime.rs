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
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::host::AgentHostHandle;
use crate::instance_lock::InstanceLock;
use crate::mcp_control::ControlServerHandle;

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

/// Mint durable-write authority for the process-wide runtime home.
///
/// * A live runtime owns this home → its authority, serialized against its
///   seal, so the write can never race the owner or outlive it.
/// * A runtime owns it but has sealed or closed → `None`. It is handing the
///   home over, and an ambient write now could land beside a replacement's.
/// * No runtime owns this home → an unowned authority. There is no second
///   writer to race, so refusing here would only strand legitimate work (a
///   library user, or a lazy credential migration in a process with no host).
///
/// `None` means "skip this durable write", never "write anyway".
pub(crate) fn current_durable_write(operation: &str) -> Option<DurableWriteGuard> {
    let lock_path = crate::discover::grokptah_home().join(".instance.lock");
    let owner = {
        let mut owners = CURRENT_OWNERS.lock();
        // Opportunistically drop entries whose runtime is fully gone.
        owners.retain(|_, weak| weak.strong_count() > 0);
        owners.get(&lock_path).and_then(std::sync::Weak::upgrade)
    };
    match owner {
        Some(owner) => owner.begin_durable_write(operation).ok(),
        None => Some(DurableWriteGuard::unowned(&lock_path)),
    }
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
pub(crate) struct DurableWriteGuard {
    lifecycle: Arc<HostLifecycle>,
    /// Whether this guard is part of the in-flight count the seal waits on.
    counted: bool,
}

impl DurableWriteGuard {
    /// Authority for a home no host runtime owns.
    ///
    /// Minted only by [`current_durable_write`] after it has established that
    /// there is no live owner to race, and by crate tests that drive the store
    /// modules directly. It carries no lifecycle, so it can never re-authorize
    /// a home some runtime does own — that path always returns the owner's
    /// guard instead.
    pub(crate) fn unowned(lock_path: &std::path::Path) -> Self {
        Self {
            lifecycle: HostLifecycle::build(None, lock_path.to_path_buf()),
            counted: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn unowned_for_test() -> Self {
        Self::unowned(std::path::Path::new("/nonexistent/.instance.lock"))
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
                .insert(lifecycle.lock_path.clone(), Arc::downgrade(&lifecycle));
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

    /// Whether this process acquired the single-instance lock at startup.
    pub(crate) fn acquired_process_lock(&self) -> bool {
        self.acquired_process_lock
    }

    /// Whether the advisory OS lock is still held right now.
    pub(crate) fn process_lock_held(&self) -> bool {
        self.instance_lock.lock().is_some()
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
        let _admission = self.spawn_gate.read();
        self.ensure_open(operation)?;
        Ok(self.tasks.spawn(future))
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
    ) -> Result<tokio_util::task::task_tracker::TrackedFuture<F>>
    where
        F: std::future::Future,
    {
        let _admission = self.spawn_gate.read();
        self.ensure_open(operation)?;
        Ok(self.tasks.track_future(future))
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
        let mut in_flight = self.in_flight_writes.lock();
        if self.writes_sealed.load(Ordering::Acquire) {
            bail!(
                "GrokPtah host runtime for {} has released durable-write authority; \
                 {operation} is refused so it cannot race the process that owns the home now",
                self.lock_path.display()
            );
        }
        self.ensure_open(operation)?;
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
    /// Supervised tasks still tracked at the moment admissions closed.
    pub supervised_tasks_at_quiesce: usize,
    /// Supervised tasks still tracked after the join barrier. Non-zero only
    /// when `join_timed_out` is set.
    pub supervised_tasks_remaining: usize,
    /// True when supervised work did not finish inside the join timeout. The
    /// process lock is then released only if durable writes still sealed.
    pub join_timed_out: bool,
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
            "UNCLEAN: joinTimedOut={} tasksRemaining={} writesSealed={} writesInFlight={} \
             lockRetainedForSafety={} errors={:?}",
            self.join_timed_out,
            self.supervised_tasks_remaining,
            self.durable_writes_sealed,
            self.durable_writes_in_flight,
            self.process_lock_retained_for_safety,
            self.flush_errors,
        )
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
    ) -> anyhow::Result<tokio_util::task::task_tracker::TrackedFuture<F>>
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
    /// 4. Seal durable-write authority and wait for in-flight writes, bounded.
    /// 5. Flush durable state and run shutdown hooks; record every failure.
    /// 6. Mark closed, then release the advisory OS lock exactly once —
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
        let servers: Vec<ControlServerHandle> = self.control_servers.lock().drain(..).collect();
        let control_servers_stopped = servers.len();
        for server in servers {
            server.stop_and_wait().await;
        }

        // 3. Cancel every in-flight unit of work, then join the supervised set.
        self.handle.cancel_all_activity().await;
        self.lifecycle.cancel_token().cancel();
        let supervised_tasks_at_quiesce = self.lifecycle.tasks().len();
        let join_timed_out = tokio::time::timeout(self.join_timeout, self.lifecycle.tasks().wait())
            .await
            .is_err();
        let supervised_tasks_remaining = self.lifecycle.tasks().len();

        // 4. Seal durable-write authority. After this no handle — stale or not
        //    — can mutate this home again.
        let write_seal_timeout = self.write_seal_timeout;
        let lifecycle = self.lifecycle.clone();
        let durable_writes_sealed =
            tokio::task::spawn_blocking(move || lifecycle.seal_durable_writes(write_seal_timeout))
                .await
                .unwrap_or(false);
        let durable_writes_in_flight = self.lifecycle.in_flight_durable_writes();

        // 5. Flush durable state and run teardown hooks. The flush runs under
        //    the seal because it is this runtime's own last write.
        let mut flush_errors = self.handle.flush_durable_state();
        let hooks: Vec<(String, ShutdownHook)> = self.hooks.lock().drain(..).collect();
        let hooks_run = hooks.len();
        for (name, hook) in hooks {
            if let Err(error) = hook() {
                flush_errors.push(format!("shutdown hook {name} failed: {error:#}"));
            }
        }
        if join_timed_out {
            flush_errors.push(format!(
                "{supervised_tasks_remaining} supervised task(s) did not finish within {:?}",
                self.join_timeout
            ));
        }
        if !durable_writes_sealed {
            flush_errors.push(format!(
                "{durable_writes_in_flight} durable write(s) still in progress after {:?}; \
                 the instance lock is retained so no replacement process can write this home",
                write_seal_timeout
            ));
        }

        // 6. Stale handles must fail closed before the lock can be re-acquired.
        self.lifecycle.mark_closed();
        let process_lock_released = durable_writes_sealed && self.lifecycle.release_process_lock();
        let process_lock_held_after = self.lifecycle.process_lock_held();

        let report = HostShutdownReport {
            already_complete: false,
            control_servers_stopped,
            supervised_tasks_at_quiesce,
            supervised_tasks_remaining,
            join_timed_out,
            durable_writes_sealed,
            durable_writes_in_flight,
            flush_errors,
            hooks_run,
            process_lock_released,
            process_lock_held_after,
            process_lock_retained_for_safety: !durable_writes_sealed && process_lock_held_after,
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
    /// authority and waits, bounded, for the writes already running. The lock
    /// is released only if that seal holds. If it does not, the lock is
    /// **retained** and a replacement process is refused, which is the safe
    /// outcome; the OS reclaims it when this process exits.
    ///
    /// Callers that need the full join barrier must await `shutdown()`.
    fn drop(&mut self) {
        if self.lifecycle.phase() == HostPhase::Closed && self.lifecycle.durable_writes_sealed() {
            return;
        }
        self.lifecycle.begin_quiesce();
        for server in self.control_servers.get_mut().drain(..) {
            server.stop();
        }
        self.lifecycle.cancel_token().cancel();
        let outstanding = self.lifecycle.tasks().len();
        self.lifecycle.seal_task_admission();
        self.lifecycle.mark_closed();

        let sealed = self.lifecycle.seal_durable_writes(self.write_seal_timeout);
        if !sealed {
            eprintln!(
                "[grokptah] host runtime for {} dropped with {} durable write(s) still in \
                 progress; the instance lock is RETAINED so no replacement process can write \
                 this home. Await HostRuntime::shutdown() for an ordered stop.",
                self.lifecycle.lock_path().display(),
                self.lifecycle.in_flight_durable_writes()
            );
            return;
        }
        if outstanding > 0 {
            eprintln!(
                "[grokptah] host runtime for {} dropped with {outstanding} supervised task(s) \
                 still running; durable writes are sealed, so the lock release is safe, but \
                 await HostRuntime::shutdown() for an ordered stop.",
                self.lifecycle.lock_path().display()
            );
        }
        self.lifecycle.release_process_lock();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn lifecycle(dir: &std::path::Path) -> Arc<HostLifecycle> {
        let home = crate::discover::RuntimeHome::from_path(dir).expect("runtime home");
        let path = home.instance_lock_path();
        let lock = InstanceLock::try_acquire_at(&home).expect("acquire instance lock");
        HostLifecycle::new(Some(lock), path)
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

        // No durable write is in flight, so the seal still holds and releasing
        // the lock is safe even though a task is stuck.
        assert!(lifecycle.seal_durable_writes(std::time::Duration::from_millis(50)));
        lifecycle.mark_closed();
        assert!(lifecycle.release_process_lock());
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

        // No owner: an ambient write is safe, because nothing can race it.
        assert!(current_durable_write("no owner").is_some());

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
        assert!(lifecycle.acquired_process_lock());
        assert!(lifecycle.process_lock_held());
        assert!(lifecycle.release_process_lock());
        assert!(!lifecycle.release_process_lock());
        assert!(!lifecycle.process_lock_held());
        assert!(dir.path().join(".instance.lock").is_file());

        let never_owned = HostLifecycle::new(None, dir.path().join(".instance.lock"));
        assert!(!never_owned.acquired_process_lock());
        assert!(!never_owned.release_process_lock());
    }
}
