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

use super::store::OrchStore;
use super::workload::WorkloadReconciliationReport;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;

pub const DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(5);

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

    pub fn stop_and_wait(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

impl Drop for WorkloadSupervisor {
    fn drop(&mut self) {
        self.stop_and_wait();
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
