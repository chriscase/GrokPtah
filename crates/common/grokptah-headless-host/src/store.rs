//! Durable run records, the idempotency ledger, and restart recovery.
//!
//! Every durable write is atomic: content goes to a temporary file, is flushed,
//! and is renamed over the target. A crash therefore leaves either the previous
//! record or the new one, never a half-written record that a later start would
//! read as truth.
//!
//! Opening the store is also the recovery point. A run that was queued or
//! running when the process ended is marked `interrupted` and is *never*
//! resumed automatically: an unattended host that silently restarts model work
//! after a crash is exactly the behavior ADR-002 withholds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use grokptah_agent_sdk::run::ExecutionMode;
use grokptah_agent_sdk::{DurableRunState, RunScope};
use serde::{Deserialize, Serialize};

use crate::attention::AttentionRecord;
use crate::authority::ResolvedBounds;
use crate::engine::{DispatchDisposition, DispatchReport};
use crate::error::{HostError, HostResult, io_error};
use crate::identity::ExternalRef;
use crate::journal::Journal;

/// Directory holding one subdirectory per run.
const RUNS_DIR: &str = "runs";
/// File holding the idempotency ledger.
const LEDGER_FILE: &str = "idempotency.json";
/// File holding one run's durable record.
const RECORD_FILE: &str = "record.json";
/// File holding one run's event journal.
const EVENTS_FILE: &str = "events.jsonl";

/// Host lifecycle phase for one run.
///
/// This is a superset of the public [`DurableRunState`]: `paused` and
/// `needs_attention` are distinct host states that both project to the public
/// `interrupted`, because from a consumer's point of view both describe a run
/// that is halted and needs an explicit operator action before it moves again.
/// The host status projection always carries the exact phase alongside the
/// public state, so the narrower public value never hides which one it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Admitted and waiting for an execution slot.
    Queued,
    /// Executing.
    Running,
    /// Halted by an operator; resumable.
    Paused,
    /// Halted by a raised escalation; resumable only once resolved.
    NeedsAttention,
    /// Finished successfully.
    Completed,
    /// Finished unsuccessfully.
    Failed,
    /// Cancelled by an operator.
    Cancelled,
    /// Interrupted by a restart; requires an explicit operator decision.
    Interrupted,
    /// Stopped at a configured bound.
    LimitReached,
}

impl RunPhase {
    /// Project into the public durable state.
    pub fn durable_state(self) -> DurableRunState {
        match self {
            Self::Queued => DurableRunState::Queued,
            Self::Running => DurableRunState::Running,
            Self::Paused | Self::NeedsAttention | Self::Interrupted => DurableRunState::Interrupted,
            Self::Completed => DurableRunState::Completed,
            Self::Failed => DurableRunState::Failed,
            Self::Cancelled => DurableRunState::Cancelled,
            Self::LimitReached => DurableRunState::LimitReached,
        }
    }

    /// Whether the run can never move again.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::LimitReached
        )
    }

    /// Whether the run is halted awaiting an explicit operator action.
    pub fn is_halted(self) -> bool {
        matches!(
            self,
            Self::Paused | Self::NeedsAttention | Self::Interrupted
        )
    }

    /// Stable label for events and receipts.
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::NeedsAttention => "needs_attention",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::LimitReached => "limit_reached",
        }
    }
}

/// What a dispatch established, once it came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchSettlement {
    /// Whether the run may advance.
    pub disposition: DispatchDisposition,
    /// Opaque reference to the orchestrator's attempt record, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<ExternalRef>,
    /// Opaque reference to the orchestrator's operation receipt, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ExternalRef>,
    /// When the dispatch settled, RFC3339.
    pub settled_at: String,
}

/// The most recent dispatch this run made, written before it happened.
///
/// The record exists on disk *before* the engine is invoked, so a process that
/// dies mid-step leaves proof that a dispatch was in flight. Writing it
/// afterwards would make an interrupted dispatch indistinguishable from one
/// that never started — and those two need opposite handling.
///
/// Only the latest dispatch is kept here; the journal carries the history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchRecord {
    /// One-based ordinal, unique and increasing within the run.
    pub ordinal: u32,
    /// The round this dispatch was taken for.
    pub round: u16,
    /// When the dispatch started, RFC3339.
    pub started_at: String,
    /// What it established. `None` means it is still in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<DispatchSettlement>,
}

impl DispatchRecord {
    /// Record that a dispatch is about to happen.
    pub fn started(ordinal: u32, round: u16, started_at: String) -> Self {
        Self {
            ordinal,
            round,
            started_at,
            settled: None,
        }
    }

    /// Whether this dispatch has not come back.
    pub fn is_in_flight(&self) -> bool {
        self.settled.is_none()
    }

    /// What the dispatch established, if it settled.
    pub fn disposition(&self) -> Option<DispatchDisposition> {
        self.settled.as_ref().map(|settled| settled.disposition)
    }

    /// Whether this run must not move until a human reconciles it.
    ///
    /// True while a dispatch is in flight as well as after an indeterminate
    /// one: an in-flight record read from disk means the process died mid-step,
    /// which is the same unanswerable question.
    pub fn blocks_progress(&self) -> bool {
        !matches!(
            self.disposition(),
            Some(
                DispatchDisposition::Local
                    | DispatchDisposition::NotDispatched
                    | DispatchDisposition::Resolved
            )
        )
    }

    /// Settle the dispatch from what the engine reported.
    ///
    /// A report carrying a reference that is not bounded settles as
    /// indeterminate: an unusable reference cannot be reconciled, and treating
    /// it as a clean result would hide that.
    pub fn settle(&mut self, report: DispatchReport, settled_at: String) {
        let bounded = report.refs_are_bounded();
        self.settled = Some(DispatchSettlement {
            disposition: if bounded {
                report.disposition
            } else {
                DispatchDisposition::Indeterminate
            },
            attempt: bounded.then_some(report.attempt).flatten(),
            receipt: bounded.then_some(report.receipt).flatten(),
            settled_at,
        });
    }

    /// Settle a dispatch that was interrupted, with nothing proven either way.
    pub fn settle_indeterminate(&mut self, settled_at: String) {
        self.settled = Some(DispatchSettlement {
            disposition: DispatchDisposition::Indeterminate,
            attempt: None,
            receipt: None,
            settled_at,
        });
    }
}

/// One reviewable changed file recorded at completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangedFileRecord {
    /// Validated repository-relative path.
    pub path: String,
    /// Bounded, redacted summary.
    pub summary: String,
}

/// Evidence recorded when a run completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletionRecord {
    /// Files the run changed.
    pub changed_files: Vec<ChangedFileRecord>,
    /// Bounded, redacted diff.
    pub diff: String,
    /// Whether the stored diff was shortened.
    pub diff_truncated: bool,
    /// Final workspace fingerprint reported by the engine.
    pub fingerprint: String,
}

/// One durable run record.
///
/// The full prompt is deliberately absent. Only a bounded, redacted preview and
/// a fingerprint are durable, so a host home never accumulates transcripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRecord {
    /// Opaque run identity.
    pub run_id: String,
    /// Owning session identity.
    pub session_id: String,
    /// Approved workspace identity.
    pub workspace: String,
    /// Caller idempotency key.
    pub request_id: String,
    /// Host lifecycle phase.
    pub phase: RunPhase,
    /// Bounded, redacted prompt preview.
    pub prompt_preview: String,
    /// Fingerprint of the admitted request, for idempotency conflicts.
    pub request_fingerprint: String,
    /// Creation timestamp, RFC3339.
    pub created_at: String,
    /// Last update timestamp, RFC3339.
    pub updated_at: String,
    /// Monotonic revision; every state change bumps it.
    pub revision: u64,
    /// Rounds already spent.
    pub rounds_used: u16,
    /// Admitted ceilings.
    pub bounds: ResolvedBounds,
    /// Shared or isolated execution.
    pub execution_mode: ExecutionMode,
    /// Epoch milliseconds when execution first started.
    pub started_at_ms: Option<u64>,
    /// Steering directives accepted but not yet delivered.
    #[serde(default)]
    pub pending_steering: Vec<String>,
    /// Open escalation, if any.
    #[serde(default)]
    pub attention: Option<AttentionRecord>,
    /// Stable reason the run stopped moving. Set for terminal runs and for a
    /// run halted by recovery; `None` while a run is still making progress.
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Completion evidence, present only for a completed run.
    #[serde(default)]
    pub completion: Option<CompletionRecord>,
    /// The most recent dispatch, written before the engine was invoked.
    #[serde(default)]
    pub dispatch: Option<DispatchRecord>,
}

impl RunRecord {
    /// The exact identity fence for this run.
    pub fn scope(&self) -> RunScope {
        RunScope {
            session_id: self.session_id.clone(),
            workspace: self.workspace.clone(),
            run_id: self.run_id.clone(),
        }
    }

    /// Move to a new phase, bumping the revision that fences control leases.
    pub fn transition(&mut self, phase: RunPhase, updated_at: String) {
        self.phase = phase;
        self.updated_at = updated_at;
        self.revision = self.revision.saturating_add(1);
    }

    /// The ordinal the next dispatch for this run must carry.
    pub fn next_dispatch_ordinal(&self) -> u32 {
        self.dispatch
            .as_ref()
            .map_or(0, |dispatch| dispatch.ordinal)
            .saturating_add(1)
    }

    /// Whether an unsettled or indeterminate dispatch blocks this run.
    pub fn dispatch_blocks_progress(&self) -> bool {
        self.dispatch
            .as_ref()
            .is_some_and(DispatchRecord::blocks_progress)
    }
}

/// One idempotency ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerEntry {
    /// Run created by the original request.
    pub run_id: String,
    /// Fingerprint of the original request payload.
    pub fingerprint: String,
}

/// What opening the store had to repair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    /// Runs marked interrupted because the host stopped while they were live.
    pub interrupted: Vec<String>,
    /// Runs whose journal had a torn trailing write discarded.
    pub torn_journals: Vec<String>,
    /// Runs still paused from a previous graceful shutdown.
    pub resumable: Vec<String>,
    /// Runs whose dispatch was in flight when the host stopped. Whether that
    /// work reached its destination is unknown, so these are never resumed.
    pub indeterminate_dispatch: Vec<String>,
}

impl RecoveryReport {
    /// Whether the previous shutdown left anything to repair.
    pub fn is_clean(&self) -> bool {
        self.interrupted.is_empty()
            && self.torn_journals.is_empty()
            && self.indeterminate_dispatch.is_empty()
    }
}

/// Durable run storage for one host home.
#[derive(Debug)]
pub struct Store {
    home: PathBuf,
    retention: usize,
    records: BTreeMap<String, RunRecord>,
    journals: BTreeMap<String, Journal>,
    ledger: BTreeMap<String, LedgerEntry>,
}

impl Store {
    /// Open the store and perform restart recovery.
    pub fn open(home: &Path, retention: usize, now: &str) -> HostResult<(Self, RecoveryReport)> {
        let runs_dir = home.join(RUNS_DIR);
        std::fs::create_dir_all(&runs_dir).map_err(|error| io_error("home_unwritable", &error))?;

        let mut store = Self {
            home: home.to_path_buf(),
            retention,
            records: BTreeMap::new(),
            journals: BTreeMap::new(),
            ledger: BTreeMap::new(),
        };
        store.ledger = load_ledger(&home.join(LEDGER_FILE))?;

        let mut run_ids = Vec::new();
        let entries =
            std::fs::read_dir(&runs_dir).map_err(|error| io_error("home_unreadable", &error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io_error("home_unreadable", &error))?;
            if entry.path().is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                run_ids.push(name.to_owned());
            }
        }
        run_ids.sort();

        let mut report = RecoveryReport::default();
        for run_id in run_ids {
            let record_path = runs_dir.join(&run_id).join(RECORD_FILE);
            if !record_path.exists() {
                continue;
            }
            let raw = std::fs::read_to_string(&record_path)
                .map_err(|error| io_error("record_unreadable", &error))?;
            let mut record: RunRecord = serde_json::from_str(&raw).map_err(|_| {
                HostError::internal("record_corrupt", "a durable run record is unreadable")
            })?;
            if record.run_id != run_id {
                return Err(HostError::internal(
                    "record_corrupt",
                    "a durable run record does not match its directory",
                ));
            }

            let journal = Journal::open(&runs_dir.join(&run_id).join(EVENTS_FILE), retention)?;
            if journal.truncated_tail() {
                report.torn_journals.push(run_id.clone());
            }

            // A dispatch left in flight is settled first, and is decisive: it
            // interrupts the run whatever phase the record claims, because the
            // question it leaves open — did that work already happen? — is not
            // one any phase can answer.
            let interrupted_dispatch = record
                .dispatch
                .as_ref()
                .is_some_and(DispatchRecord::is_in_flight);
            if interrupted_dispatch && let Some(dispatch) = record.dispatch.as_mut() {
                dispatch.settle_indeterminate(now.to_owned());
                report.indeterminate_dispatch.push(run_id.clone());
            }

            match record.phase {
                RunPhase::Running | RunPhase::Queued => {
                    record.transition(RunPhase::Interrupted, now.to_owned());
                    record.stop_reason = Some("restart_recovery".to_owned());
                    record.pending_steering.clear();
                    report.interrupted.push(run_id.clone());
                }
                RunPhase::Paused | RunPhase::NeedsAttention if interrupted_dispatch => {
                    record.transition(RunPhase::Interrupted, now.to_owned());
                    record.stop_reason = Some("dispatch_indeterminate".to_owned());
                    record.pending_steering.clear();
                    report.interrupted.push(run_id.clone());
                }
                RunPhase::Paused | RunPhase::NeedsAttention => {
                    report.resumable.push(run_id.clone());
                }
                _ if interrupted_dispatch => {
                    record
                        .stop_reason
                        .get_or_insert_with(|| "dispatch_indeterminate".to_owned());
                }
                _ => {}
            }

            store.journals.insert(run_id.clone(), journal);
            store.records.insert(run_id.clone(), record);
        }

        let mut repaired: Vec<&String> = report
            .interrupted
            .iter()
            .chain(report.indeterminate_dispatch.iter())
            .collect();
        repaired.sort();
        repaired.dedup();
        for run_id in repaired {
            store.persist_record(run_id)?;
        }

        Ok((store, report))
    }

    /// Insert a freshly admitted run.
    pub fn insert(&mut self, record: RunRecord) -> HostResult<()> {
        let run_id = record.run_id.clone();
        let dir = self.run_dir(&run_id);
        std::fs::create_dir_all(&dir).map_err(|error| io_error("home_unwritable", &error))?;
        let journal = Journal::open(&dir.join(EVENTS_FILE), self.retention)?;
        self.journals.insert(run_id.clone(), journal);
        self.records.insert(run_id.clone(), record);
        self.persist_record(&run_id)
    }

    /// Read one run record.
    pub fn get(&self, run_id: &str) -> HostResult<&RunRecord> {
        self.records.get(run_id).ok_or_else(unknown_run)
    }

    /// Mutate one run record. The caller must persist afterwards.
    pub fn get_mut(&mut self, run_id: &str) -> HostResult<&mut RunRecord> {
        self.records.get_mut(run_id).ok_or_else(unknown_run)
    }

    /// One run's event journal.
    pub fn journal(&self, run_id: &str) -> HostResult<&Journal> {
        self.journals.get(run_id).ok_or_else(unknown_run)
    }

    /// One run's event journal, for appending.
    pub fn journal_mut(&mut self, run_id: &str) -> HostResult<&mut Journal> {
        self.journals.get_mut(run_id).ok_or_else(unknown_run)
    }

    /// All run identities, oldest identity order.
    pub fn run_ids(&self) -> Vec<String> {
        self.records.keys().cloned().collect()
    }

    /// Every run record.
    pub fn records(&self) -> impl Iterator<Item = &RunRecord> {
        self.records.values()
    }

    /// Count runs in a phase.
    pub fn count_phase(&self, phase: RunPhase) -> usize {
        self.records
            .values()
            .filter(|record| record.phase == phase)
            .count()
    }

    /// Write one run record atomically.
    pub fn persist_record(&self, run_id: &str) -> HostResult<()> {
        let record = self.get(run_id)?;
        let dir = self.run_dir(run_id);
        std::fs::create_dir_all(&dir).map_err(|error| io_error("home_unwritable", &error))?;
        let encoded = serde_json::to_vec_pretty(record).map_err(|_| {
            HostError::internal("record_unserializable", "run record cannot be persisted")
        })?;
        write_atomic(
            &dir.join("record.json.tmp"),
            &dir.join(RECORD_FILE),
            &encoded,
        )
    }

    /// Look up a previous request by its idempotency key.
    pub fn ledger_lookup(&self, request_id: &str) -> Option<&LedgerEntry> {
        self.ledger.get(request_id)
    }

    /// Record an idempotency key for a newly created run.
    pub fn ledger_record(
        &mut self,
        request_id: &str,
        run_id: &str,
        fingerprint: &str,
    ) -> HostResult<()> {
        self.ledger.insert(
            request_id.to_owned(),
            LedgerEntry {
                run_id: run_id.to_owned(),
                fingerprint: fingerprint.to_owned(),
            },
        );
        let encoded = serde_json::to_vec_pretty(&self.ledger).map_err(|_| {
            HostError::internal("ledger_unserializable", "ledger cannot be persisted")
        })?;
        write_atomic(
            &self.home.join("idempotency.json.tmp"),
            &self.home.join(LEDGER_FILE),
            &encoded,
        )
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.home.join(RUNS_DIR).join(run_id)
    }
}

fn unknown_run() -> HostError {
    HostError::not_found("run_unknown", "no such run for this session")
}

fn load_ledger(path: &Path) -> HostResult<BTreeMap<String, LedgerEntry>> {
    match std::fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Ok(BTreeMap::new()),
        Ok(raw) => serde_json::from_str(&raw).map_err(|_| {
            HostError::internal("ledger_corrupt", "the idempotency ledger is unreadable")
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(io_error("ledger_unreadable", &error)),
    }
}

/// Write bytes to `temp`, flush, then rename over `final_path`.
pub(crate) fn write_atomic(temp: &Path, final_path: &Path, bytes: &[u8]) -> HostResult<()> {
    use std::io::Write;

    {
        let mut file =
            std::fs::File::create(temp).map_err(|error| io_error("record_unwritable", &error))?;
        file.write_all(bytes)
            .map_err(|error| io_error("record_unwritable", &error))?;
        file.sync_data()
            .map_err(|error| io_error("record_unwritable", &error))?;
    }
    std::fs::rename(temp, final_path).map_err(|error| io_error("record_unwritable", &error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn record(run_id: &str, phase: RunPhase) -> RunRecord {
        testing::run_record_fixture(run_id, phase)
    }

    #[test]
    fn phases_project_truthfully_and_name_what_is_halted() {
        assert_eq!(
            RunPhase::Paused.durable_state(),
            DurableRunState::Interrupted
        );
        assert_eq!(
            RunPhase::NeedsAttention.durable_state(),
            DurableRunState::Interrupted
        );
        assert!(RunPhase::Paused.is_halted());
        assert!(!RunPhase::Paused.is_terminal());
        assert!(RunPhase::Completed.is_terminal());
        assert_eq!(RunPhase::LimitReached.label(), "limit_reached");
    }

    #[test]
    fn live_runs_become_interrupted_on_reopen_and_are_never_resumed() {
        let home = tempfile::tempdir().expect("temp home");
        {
            let (mut store, report) =
                Store::open(home.path(), 32, testing::TS).expect("store opens");
            assert!(report.is_clean());
            store
                .insert(record("run-live", RunPhase::Running))
                .expect("insert");
            store
                .insert(record("run-queued", RunPhase::Queued))
                .expect("insert");
            store
                .insert(record("run-paused", RunPhase::Paused))
                .expect("insert");
            store
                .insert(record("run-done", RunPhase::Completed))
                .expect("insert");
        }

        let (store, report) = Store::open(home.path(), 32, testing::TS).expect("store reopens");
        assert_eq!(report.interrupted, vec!["run-live", "run-queued"]);
        assert_eq!(report.resumable, vec!["run-paused"]);
        assert!(!report.is_clean());

        assert_eq!(
            store.get("run-live").expect("run").phase,
            RunPhase::Interrupted
        );
        assert_eq!(
            store.get("run-live").expect("run").stop_reason.as_deref(),
            Some("restart_recovery")
        );
        assert_eq!(
            store.get("run-paused").expect("run").phase,
            RunPhase::Paused
        );
        assert_eq!(
            store.get("run-done").expect("run").phase,
            RunPhase::Completed
        );
        assert_eq!(store.count_phase(RunPhase::Interrupted), 2);
    }

    #[test]
    fn recovery_is_recorded_durably_so_it_is_not_repeated() {
        let home = tempfile::tempdir().expect("temp home");
        {
            let (mut store, _) = Store::open(home.path(), 32, testing::TS).expect("store opens");
            store
                .insert(record("run-live", RunPhase::Running))
                .expect("insert");
        }
        let first = Store::open(home.path(), 32, testing::TS).expect("reopen").1;
        assert_eq!(first.interrupted.len(), 1);
        let second = Store::open(home.path(), 32, testing::TS).expect("reopen").1;
        assert!(second.interrupted.is_empty(), "recovery must be durable");
    }

    #[test]
    fn a_dispatch_left_in_flight_is_settled_indeterminate_and_interrupts_the_run() {
        let home = tempfile::tempdir().expect("temp home");
        {
            let (mut store, _) = Store::open(home.path(), 32, testing::TS).expect("store opens");
            let mut live = record("run-dispatching", RunPhase::Running);
            live.dispatch = Some(DispatchRecord::started(1, 1, testing::TS.into()));
            store.insert(live).expect("insert");

            // A paused run is normally resumable, but not with a dispatch that
            // never came back.
            let mut paused = record("run-paused-mid-dispatch", RunPhase::Paused);
            paused.dispatch = Some(DispatchRecord::started(3, 2, testing::TS.into()));
            store.insert(paused).expect("insert");
        }

        let (store, report) = Store::open(home.path(), 32, testing::TS).expect("store reopens");
        assert_eq!(
            report.indeterminate_dispatch,
            vec!["run-dispatching", "run-paused-mid-dispatch"]
        );
        assert!(report.resumable.is_empty());
        assert!(!report.is_clean());

        for run_id in ["run-dispatching", "run-paused-mid-dispatch"] {
            let record = store.get(run_id).expect("run");
            assert_eq!(record.phase, RunPhase::Interrupted);
            let dispatch = record.dispatch.as_ref().expect("dispatch");
            assert!(!dispatch.is_in_flight());
            assert_eq!(
                dispatch.disposition(),
                Some(DispatchDisposition::Indeterminate)
            );
            assert!(record.dispatch_blocks_progress());
        }
        assert_eq!(
            store
                .get("run-dispatching")
                .expect("run")
                .next_dispatch_ordinal(),
            2
        );
    }

    #[test]
    fn a_settled_dispatch_survives_reopen_without_being_reopened() {
        let home = tempfile::tempdir().expect("temp home");
        {
            let (mut store, _) = Store::open(home.path(), 32, testing::TS).expect("store opens");
            let mut done = record("run-done", RunPhase::Completed);
            let mut dispatch = DispatchRecord::started(1, 1, testing::TS.into());
            dispatch.settle(
                DispatchReport::external(
                    DispatchDisposition::Resolved,
                    ExternalRef::new("attempt-1"),
                    ExternalRef::new("receipt-1"),
                ),
                testing::TS.into(),
            );
            done.dispatch = Some(dispatch);
            store.insert(done).expect("insert");
        }

        let (store, report) = Store::open(home.path(), 32, testing::TS).expect("store reopens");
        assert!(report.is_clean());
        let record = store.get("run-done").expect("run");
        assert!(!record.dispatch_blocks_progress());
        let settled = record.dispatch.as_ref().expect("dispatch").settled.as_ref();
        assert_eq!(
            settled
                .expect("settlement")
                .attempt
                .as_ref()
                .map(ExternalRef::as_str),
            Some("attempt-1")
        );
    }

    #[test]
    fn an_unusable_reference_settles_indeterminate_rather_than_clean() {
        let mut dispatch = DispatchRecord::started(1, 1, testing::TS.into());
        let smuggled: ExternalRef =
            serde_json::from_str("\"../escape\"").expect("serde is transparent");
        dispatch.settle(
            DispatchReport::external(DispatchDisposition::Resolved, Some(smuggled), None),
            testing::TS.into(),
        );
        assert_eq!(
            dispatch.disposition(),
            Some(DispatchDisposition::Indeterminate)
        );
        assert!(
            dispatch
                .settled
                .as_ref()
                .expect("settlement")
                .attempt
                .is_none(),
            "an unusable reference is not recorded"
        );
        assert!(dispatch.blocks_progress());
    }

    #[test]
    fn the_ledger_survives_a_restart() {
        let home = tempfile::tempdir().expect("temp home");
        {
            let (mut store, _) = Store::open(home.path(), 32, testing::TS).expect("store opens");
            store
                .insert(record("run-1", RunPhase::Completed))
                .expect("insert");
            store
                .ledger_record("req-1", "run-1", "fp-1")
                .expect("ledger");
        }
        let (store, _) = Store::open(home.path(), 32, testing::TS).expect("store reopens");
        let entry = store.ledger_lookup("req-1").expect("entry survives");
        assert_eq!(entry.run_id, "run-1");
        assert_eq!(entry.fingerprint, "fp-1");
        assert!(store.ledger_lookup("req-absent").is_none());
    }

    #[test]
    fn a_corrupt_record_fails_closed_instead_of_being_skipped() {
        let home = tempfile::tempdir().expect("temp home");
        let dir = home.path().join(RUNS_DIR).join("run-bad");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(RECORD_FILE), "{\"runId\":\"run-bad\"}").expect("write");
        let error = Store::open(home.path(), 32, testing::TS).expect_err("corrupt record");
        assert_eq!(error.reason_code(), "record_corrupt");
    }

    #[test]
    fn unknown_runs_are_not_found_rather_than_internal_errors() {
        let home = tempfile::tempdir().expect("temp home");
        let (store, _) = Store::open(home.path(), 32, testing::TS).expect("store opens");
        let error = store.get("run-absent").expect_err("unknown run");
        assert_eq!(error.reason_code(), "run_unknown");
        assert_eq!(error.code(), grokptah_agent_sdk::ErrorCode::NotFound);
    }
}
