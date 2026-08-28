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
}

impl HostLifecycle {
    pub(crate) fn new(instance_lock: Option<InstanceLock>, lock_path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            phase: AtomicU8::new(HostPhase::Running.as_u8()),
            cancel: CancellationToken::new(),
            tasks: TaskTracker::new(),
            acquired_process_lock: instance_lock.is_some(),
            instance_lock: Mutex::new(instance_lock),
            lock_path,
            spawn_gate: parking_lot::RwLock::new(()),
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

    /// Arm the join barrier. After this returns no further supervised task can
    /// be registered, because every spawner re-checks the phase under the same
    /// gate.
    fn seal_task_admission(&self) {
        let _sealed = self.spawn_gate.write();
        self.tasks.close();
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
    }

    /// Release the advisory OS lock exactly once. The lock *file* stays on
    /// disk; only the advisory lock is dropped.
    fn release_process_lock(&self) -> bool {
        let held = self.instance_lock.lock().take();
        held.is_some()
    }
}

/// What one [`HostRuntime::shutdown`] call actually did.
///
/// This is the shutdown-ordering proof surface: tests and operators can assert
/// that supervised work was joined and that the process lock was released
/// while the lock file remained on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostShutdownReport {
    /// True when a previous call already completed the ordered shutdown.
    pub already_complete: bool,
    /// Control servers whose accept loop was cancelled and joined.
    pub control_servers_stopped: usize,
    /// Supervised tasks still tracked at the moment admissions closed.
    pub supervised_tasks_at_quiesce: usize,
    /// Supervised tasks still tracked after the join barrier. Always 0 for a
    /// successful ordered shutdown.
    pub supervised_tasks_remaining: usize,
    /// True when this call is the one that released the advisory OS lock.
    pub process_lock_released: bool,
    /// Advisory lock state after shutdown. Always false once complete.
    pub process_lock_held_after: bool,
    /// The lock file must survive shutdown; only the advisory lock is released.
    pub lock_file_present: bool,
    /// Terminal phase (always [`HostPhase::Closed`]).
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

    /// Hand a control server to the runtime so ordered shutdown stops HTTP/SSE
    /// acceptance *before* run tasks are cancelled and joined.
    pub fn attach_control_server(&self, server: ControlServerHandle) {
        self.control_servers.lock().push(server);
    }

    /// Ordered, idempotent shutdown.
    ///
    /// 1. Refuse new admissions (`Running` → `Quiescing`).
    /// 2. Stop HTTP/SSE acceptance and join every attached control server's
    ///    serving task and orchestration background supervisors.
    /// 3. Cancel in-flight turns, subagents, background scans, Computer Use
    ///    operations and live shells, then **join** every supervised task.
    /// 4. Flush durable state and close the shared ledgers.
    /// 5. Mark the lifecycle closed so stale handles fail closed.
    /// 6. Release the advisory OS lock exactly once, keeping the lock file.
    ///
    /// Steps 5 and 6 are in that order on purpose: no handle may mutate after
    /// the lock is available to a replacement process.
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

        // 2. Stop accepting HTTP/SSE work and join the serving tasks.
        let servers: Vec<ControlServerHandle> = self.control_servers.lock().drain(..).collect();
        let control_servers_stopped = servers.len();
        for server in servers {
            server.stop_and_wait().await;
        }

        // 3. Cancel every in-flight unit of work, then join the supervised set.
        self.handle.cancel_all_activity().await;
        self.lifecycle.cancel_token().cancel();
        let supervised_tasks_at_quiesce = self.lifecycle.tasks().len();
        self.lifecycle.seal_task_admission();
        self.lifecycle.tasks().wait().await;
        let supervised_tasks_remaining = self.lifecycle.tasks().len();

        // 4. Mark the host stopped, then flush durable state and close the
        //    shared ledgers. Every supervised writer has already joined.
        let _ = self.handle.stop();
        self.handle.flush_durable_state();

        // 5. Stale handles must fail closed before the lock can be re-acquired.
        self.lifecycle.mark_closed();

        // 6. Release the advisory lock exactly once; keep the file.
        let process_lock_released = self.lifecycle.release_process_lock();

        let report = HostShutdownReport {
            already_complete: false,
            control_servers_stopped,
            supervised_tasks_at_quiesce,
            supervised_tasks_remaining,
            process_lock_released,
            process_lock_held_after: self.lifecycle.process_lock_held(),
            lock_file_present: self.lifecycle.lock_path().exists(),
            phase: self.lifecycle.phase(),
        };
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
    /// Fail-safe for a runtime dropped without [`HostRuntime::shutdown`].
    ///
    /// `Drop` cannot await, so it cannot provide the join barrier. It still
    /// closes the lifecycle first — which makes every surviving handle fail
    /// closed — and only then releases the advisory lock, so a replacement
    /// process never races a handle that still believes it has authority.
    /// Callers that need the join barrier must await `shutdown()`.
    fn drop(&mut self) {
        if self.lifecycle.phase() == HostPhase::Closed {
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
        if outstanding > 0 {
            eprintln!(
                "[grokptah] host runtime for {} dropped with {outstanding} supervised task(s) \
                 still running; await HostRuntime::shutdown() for an ordered stop",
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
