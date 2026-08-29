//! Background reconciliation for durable workloads.
//!
//! The supervisor is deliberately a recovery loop, not a second execution
//! engine. Model turns still run through the existing host and finite Run
//! lifecycle; this loop makes leases, deadlines, and dependency admission
//! converge after a process crash or a disconnected client.

use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use std::sync::Arc as StdArc;

use super::routine::{Clock, RoutineFireReport, SystemClock};
use super::store::OrchStore;
use super::workload::WorkloadReconciliationReport;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;

pub const DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(5);
pub const DEFAULT_ROUTINE_TICK_INTERVAL: Duration = Duration::from_secs(1);
pub const DEFAULT_MANAGER_TICK_INTERVAL: Duration = Duration::from_secs(2);
pub const MAX_MANAGER_PLANS_PER_PASS: usize = 16;
pub const MAX_MANAGER_OBSERVATIONS_PER_PASS: usize = 64;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSupervisorReport {
    pub plans_scanned: usize,
    pub plans_processed: usize,
    pub work_created: usize,
    pub messages_created: usize,
    pub decisions_created: usize,
    pub decisions_applied: usize,
    pub decisions_rejected: usize,
    pub bounded: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSupervisorStatus {
    pub enabled: bool,
    pub interval_ms: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_report: ManagerSupervisorReport,
}

impl ManagerSupervisorStatus {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            interval_ms: DEFAULT_MANAGER_TICK_INTERVAL
                .as_millis()
                .min(u64::MAX as u128) as u64,
            started_at: None,
            last_run_at: None,
            last_success_at: None,
            last_error: None,
            last_report: ManagerSupervisorReport::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadSupervisorStatus {
    pub enabled: bool,
    pub interval_ms: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_report: WorkloadReconciliationReport,
}

impl WorkloadSupervisorStatus {
    pub fn disabled(interval: Duration) -> Self {
        Self {
            enabled: false,
            interval_ms: interval.as_millis().min(u64::MAX as u128) as u64,
            started_at: None,
            last_run_at: None,
            last_success_at: None,
            last_error: None,
            last_report: WorkloadReconciliationReport::default(),
        }
    }
}

struct SupervisorState {
    status: WorkloadSupervisorStatus,
}

pub struct WorkloadSupervisor {
    stop: Option<SyncSender<()>>,
    task: Option<JoinHandle<()>>,
    state: Arc<Mutex<SupervisorState>>,
}

impl WorkloadSupervisor {
    /// Start the shared local/hosted reconciliation loop. The pass itself is
    /// synchronous filesystem work, so the supervisor owns a small stoppable
    /// thread and can release the durable store lock synchronously during
    /// shutdown from either Tokio or desktop code.
    pub fn start(store: OrchStore, interval: Duration) -> Option<Self> {
        let interval = if interval.is_zero() {
            Duration::from_secs(1)
        } else {
            interval
        };
        let started_at = Utc::now();
        let state = Arc::new(Mutex::new(SupervisorState {
            status: WorkloadSupervisorStatus {
                enabled: true,
                interval_ms: interval.as_millis().min(u64::MAX as u128) as u64,
                started_at: Some(started_at),
                last_run_at: None,
                last_success_at: None,
                last_error: None,
                last_report: WorkloadReconciliationReport::default(),
            },
        }));

        reconcile_once(&store, &state);

        let (stop_tx, stop_rx) = sync_channel(1);
        let task_state = state.clone();
        let task = thread::Builder::new()
            .name("grokptah-workload-supervisor".into())
            .spawn(move || loop {
                match stop_rx.recv_timeout(interval) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => reconcile_once(&store, &task_state),
                }
            })
            .ok()?;

        Some(Self {
            stop: Some(stop_tx),
            task: Some(task),
            state,
        })
    }

    pub fn status(&self) -> WorkloadSupervisorStatus {
        self.state.lock().status.clone()
    }

    pub fn stop_and_wait(&mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.join().map_err(|payload| {
                format!("workload supervisor panicked: {}", panic_text(&payload))
            })?;
        }
        Ok(())
    }
}

impl Drop for WorkloadSupervisor {
    fn drop(&mut self) {
        let _ = self.stop_and_wait();
    }
}

fn reconcile_once(store: &OrchStore, state: &Arc<Mutex<SupervisorState>>) {
    let now = Utc::now();
    state.lock().status.last_run_at = Some(now);
    match store.reconcile_workloads_at(now) {
        Ok(report) => {
            let mut state = state.lock();
            state.status.last_success_at = Some(now);
            state.status.last_error = None;
            state.status.last_report = report;
        }
        Err(error) => {
            state.lock().status.last_error = Some(error.to_string());
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineSupervisorStatus {
    pub enabled: bool,
    pub interval_ms: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_report: RoutineFireReport,
}

impl RoutineSupervisorStatus {
    pub fn disabled(interval: Duration) -> Self {
        Self {
            enabled: false,
            interval_ms: interval.as_millis().min(u64::MAX as u128) as u64,
            started_at: None,
            last_run_at: None,
            last_success_at: None,
            last_error: None,
            last_report: RoutineFireReport::default(),
        }
    }
}

struct RoutineSupervisorState {
    status: RoutineSupervisorStatus,
}

/// Runtime-home owner tick loop. Desktop UI timers are not used.
pub struct RoutineSupervisor {
    stop: Option<SyncSender<()>>,
    task: Option<JoinHandle<()>>,
    state: Arc<Mutex<RoutineSupervisorState>>,
}

impl RoutineSupervisor {
    pub fn start(store: OrchStore, interval: Duration) -> Option<Self> {
        Self::start_with_clock(store, interval, StdArc::new(SystemClock))
    }

    pub fn start_with_clock(
        store: OrchStore,
        interval: Duration,
        clock: StdArc<dyn Clock>,
    ) -> Option<Self> {
        let interval = if interval.is_zero() {
            Duration::from_secs(1)
        } else {
            interval
        };
        let started_at = clock.now();
        let state = Arc::new(Mutex::new(RoutineSupervisorState {
            status: RoutineSupervisorStatus {
                enabled: true,
                interval_ms: interval.as_millis().min(u64::MAX as u128) as u64,
                started_at: Some(started_at),
                last_run_at: None,
                last_success_at: None,
                last_error: None,
                last_report: RoutineFireReport::default(),
            },
        }));
        fire_once(&store, &state, clock.as_ref());
        let (stop_tx, stop_rx) = sync_channel(1);
        let task_state = state.clone();
        let task = thread::Builder::new()
            .name("grokptah-routine-supervisor".into())
            .spawn(move || loop {
                match stop_rx.recv_timeout(interval) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {
                        fire_once(&store, &task_state, clock.as_ref())
                    }
                }
            })
            .ok()?;
        Some(Self {
            stop: Some(stop_tx),
            task: Some(task),
            state,
        })
    }

    pub fn status(&self) -> RoutineSupervisorStatus {
        self.state.lock().status.clone()
    }

    pub fn stop_and_wait(&mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.join().map_err(|payload| {
                format!("routine supervisor panicked: {}", panic_text(&payload))
            })?;
        }
        Ok(())
    }
}

impl Drop for RoutineSupervisor {
    fn drop(&mut self) {
        let _ = self.stop_and_wait();
    }
}

fn panic_text(payload: &Box<dyn std::any::Any + Send + 'static>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

fn fire_once(store: &OrchStore, state: &Arc<Mutex<RoutineSupervisorState>>, clock: &dyn Clock) {
    let now = clock.now();
    state.lock().status.last_run_at = Some(now);
    match store.fire_due_routines_at(now) {
        Ok(report) => {
            let mut state = state.lock();
            state.status.last_success_at = Some(now);
            state.status.last_error = None;
            state.status.last_report = report;
        }
        Err(error) => {
            state.lock().status.last_error = Some(error.to_string());
        }
    }
}
