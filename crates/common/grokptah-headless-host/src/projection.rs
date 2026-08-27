//! Truthful status, health, and receipt projections.
//!
//! A projection here is allowed to say less than the host knows; it is never
//! allowed to say more. Two rules enforce that.
//!
//! *Nothing is fabricated.* A receipt exists only when a run actually completed
//! and recorded evidence. A run with no recorded changes has no receipt, rather
//! than an empty one that reads as a reviewed no-op.
//!
//! *Nothing is published that the public contract would reject.* Every
//! projection is validated against the SDK contract before it leaves the host;
//! a projection that fails validation is an internal error, not a value that
//! escapes with a shrug.

use grokptah_agent_sdk::run::MAX_REVIEW_DIFF_BYTES;
use grokptah_agent_sdk::{
    ChangedFile, DurableRun, ErrorEventRange, ReviewReceipt, RunNotification, RunScope,
};
use serde::Serialize;

use crate::attention::AttentionRecord;
use crate::config::HostConfig;
use crate::error::{HostError, HostResult};
use crate::journal::Journal;
use crate::lifecycle::HostState;
use crate::redaction::HOME_LABEL;
use crate::store::{RunPhase, RunRecord, Store};

/// Operation a client polls after a cursor recovery.
pub const RECOVERY_POLL_OPERATION: &str = "events";

/// Bounded, truthful status for one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRunStatus {
    /// Public durable projection.
    pub durable: DurableRun,
    /// Exact host phase. Never hidden behind the narrower public state.
    pub phase: RunPhase,
    /// Revision a control lease must match.
    pub revision: u64,
    /// Rounds already spent.
    pub rounds_used: u16,
    /// Admitted round ceiling.
    pub max_rounds: u16,
    /// Open escalation, if any.
    pub attention: Option<AttentionRecord>,
    /// Retained event window, if any events are retained.
    pub event_range: Option<ErrorEventRange>,
    /// Stable reason the run stopped moving.
    pub stop_reason: Option<String>,
    /// Whether a review receipt exists for this run right now.
    pub receipt_available: bool,
}

/// Counts by host phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCounts {
    /// Waiting for admission.
    pub queued: usize,
    /// Executing.
    pub running: usize,
    /// Halted by an operator.
    pub paused: usize,
    /// Halted by an escalation.
    pub needs_attention: usize,
    /// Interrupted by a restart.
    pub interrupted: usize,
    /// Terminal, any outcome.
    pub terminal: usize,
}

/// Health and readiness for the whole host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    /// Contract version this host speaks.
    pub contract: String,
    /// Coarse lifecycle state.
    pub state: HostState,
    /// When the host started, RFC3339.
    pub started_at: String,
    /// Milliseconds since start.
    pub uptime_ms: u64,
    /// Session identity every run is fenced to.
    pub session_id: String,
    /// Share-safe home label.
    pub home: &'static str,
    /// Share-safe workspace alias.
    pub workspace: String,
    /// Engine label, or `none` when no engine is wired.
    pub engine: String,
    /// Whether this process owns the home lock.
    pub lock_held: bool,
    /// Run counts by phase.
    pub runs: RunCounts,
    /// Runs with an open escalation.
    pub needs_attention: Vec<String>,
    /// Runs interrupted by a restart and awaiting an operator decision.
    pub awaiting_recovery: Vec<String>,
    /// Live control leases.
    pub live_leases: usize,
    /// Reasons the host is not fully healthy. Empty means healthy.
    pub degraded: Vec<String>,
    /// Capability identifiers this host can currently honor.
    pub capabilities: Vec<String>,
}

impl HealthReport {
    /// Whether the host is ready and nothing is degraded.
    pub fn is_healthy(&self) -> bool {
        self.state == HostState::Ready && self.degraded.is_empty()
    }
}

/// Build the bounded status projection for one run.
pub fn run_status(record: &RunRecord, journal: &Journal) -> HostResult<HostRunStatus> {
    let durable = DurableRun {
        run_id: record.run_id.clone(),
        session_id: record.session_id.clone(),
        workspace: record.workspace.clone(),
        request_id: record.request_id.clone(),
        state: record.phase.durable_state(),
        prompt_preview: record.prompt_preview.clone(),
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
    };
    durable.validate().map_err(|reason| {
        HostError::internal(
            "projection_invalid",
            format!("run projection is not publishable: {reason}"),
        )
    })?;

    Ok(HostRunStatus {
        durable,
        phase: record.phase,
        revision: record.revision,
        rounds_used: record.rounds_used,
        max_rounds: record.bounds.max_rounds,
        attention: record.attention.clone(),
        event_range: journal.retained_range(),
        stop_reason: record.stop_reason.clone(),
        receipt_available: receipt_available(record),
    })
}

/// Whether a run has recorded evidence a receipt can be built from.
pub fn receipt_available(record: &RunRecord) -> bool {
    record.phase == RunPhase::Completed
        && record
            .completion
            .as_ref()
            .is_some_and(|completion| !completion.changed_files.is_empty())
}

/// Build the review receipt for a completed run.
///
/// A run that completed without changing anything has no receipt: reporting an
/// empty diff with a fingerprint would read as "reviewed, nothing to change"
/// when the truth is "there is nothing to review".
pub fn review_receipt(record: &RunRecord) -> HostResult<ReviewReceipt> {
    if record.phase != RunPhase::Completed {
        return Err(HostError::not_found(
            "receipt_absent",
            "only a completed run has a review receipt",
        ));
    }
    let completion = record.completion.as_ref().ok_or_else(|| {
        HostError::internal(
            "receipt_missing_evidence",
            "the run completed without recording evidence",
        )
    })?;
    if completion.changed_files.is_empty() {
        return Err(HostError::not_found(
            "receipt_absent",
            "the run completed without changing anything reviewable",
        ));
    }

    let receipt = ReviewReceipt {
        changed_files: completion
            .changed_files
            .iter()
            .map(|file| ChangedFile {
                path: file.path.clone(),
                summary: file.summary.clone(),
            })
            .collect(),
        diff: completion.diff.clone(),
        diff_truncated: completion.diff_truncated,
        fingerprint: completion.fingerprint.clone(),
    };
    if receipt.diff.len() > MAX_REVIEW_DIFF_BYTES {
        return Err(HostError::internal(
            "projection_invalid",
            "review diff exceeds its public bound",
        ));
    }
    receipt.validate().map_err(|reason| {
        HostError::internal(
            "projection_invalid",
            format!("review receipt is not publishable: {reason}"),
        )
    })?;
    Ok(receipt)
}

/// Build the recovery notification for an expired cursor.
pub fn recovery_notification(scope: RunScope, after_seq: u64, reason: &str) -> RunNotification {
    RunNotification::Recovery {
        scope,
        after_seq,
        reason: reason.to_owned(),
        poll_tool: RECOVERY_POLL_OPERATION.to_owned(),
    }
}

/// Count runs by host phase.
pub fn run_counts(store: &Store) -> RunCounts {
    let mut counts = RunCounts::default();
    for record in store.records() {
        match record.phase {
            RunPhase::Queued => counts.queued += 1,
            RunPhase::Running => counts.running += 1,
            RunPhase::Paused => counts.paused += 1,
            RunPhase::NeedsAttention => counts.needs_attention += 1,
            RunPhase::Interrupted => counts.interrupted += 1,
            phase if phase.is_terminal() => counts.terminal += 1,
            _ => {}
        }
    }
    counts
}

/// Assemble the health report from observed host state.
#[allow(clippy::too_many_arguments)]
pub fn health(
    config: &HostConfig,
    state: HostState,
    started_at: &str,
    uptime_ms: u64,
    engine: &str,
    lock_held: bool,
    live_leases: usize,
    store: &Store,
    capabilities: Vec<String>,
) -> HealthReport {
    let mut needs_attention = Vec::new();
    let mut awaiting_recovery = Vec::new();
    for record in store.records() {
        if record.attention.is_some() {
            needs_attention.push(record.run_id.clone());
        }
        if record.phase == RunPhase::Interrupted {
            awaiting_recovery.push(record.run_id.clone());
        }
    }

    let mut degraded = Vec::new();
    if engine == "none" {
        degraded.push("engine_disabled".to_owned());
    }
    if !lock_held {
        degraded.push("home_not_owned".to_owned());
    }
    if !needs_attention.is_empty() {
        degraded.push("runs_need_attention".to_owned());
    }
    if !awaiting_recovery.is_empty() {
        degraded.push("runs_awaiting_recovery".to_owned());
    }

    HealthReport {
        contract: grokptah_agent_sdk::CONTRACT_VERSION.to_owned(),
        state,
        started_at: started_at.to_owned(),
        uptime_ms,
        session_id: config.session_id.clone(),
        home: HOME_LABEL,
        workspace: config.workspace_alias(),
        engine: engine.to_owned(),
        lock_held,
        runs: run_counts(store),
        needs_attention,
        awaiting_recovery,
        live_leases,
        degraded,
        capabilities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ChangedFileRecord, CompletionRecord};
    use crate::testing;

    fn completed_record(changed: bool) -> RunRecord {
        let mut record = testing::run_record_fixture("run-1", RunPhase::Completed);
        record.completion = Some(CompletionRecord {
            changed_files: if changed {
                vec![ChangedFileRecord {
                    path: "src/lib.rs".into(),
                    summary: "add guard".into(),
                }]
            } else {
                Vec::new()
            },
            diff: "--- a\n+++ b\n".into(),
            diff_truncated: false,
            fingerprint: "fingerprint-1".into(),
        });
        record
    }

    #[test]
    fn a_completed_run_with_changes_has_an_exact_receipt() {
        let record = completed_record(true);
        let receipt = review_receipt(&record).expect("receipt exists");
        assert_eq!(receipt.changed_files.len(), 1);
        assert_eq!(receipt.fingerprint, "fingerprint-1");
        assert!(!receipt.diff_truncated);
        assert!(receipt_available(&record));
    }

    #[test]
    fn a_run_without_evidence_has_no_receipt_rather_than_an_empty_one() {
        let no_changes = completed_record(false);
        assert_eq!(
            review_receipt(&no_changes)
                .expect_err("no reviewable evidence")
                .reason_code(),
            "receipt_absent"
        );
        assert!(!receipt_available(&no_changes));

        let running = testing::run_record_fixture("run-2", RunPhase::Running);
        assert_eq!(
            review_receipt(&running)
                .expect_err("an unfinished run has no receipt")
                .reason_code(),
            "receipt_absent"
        );

        let mut completed_without_evidence = completed_record(true);
        completed_without_evidence.completion = None;
        assert_eq!(
            review_receipt(&completed_without_evidence)
                .expect_err("completion without evidence is an internal fault")
                .reason_code(),
            "receipt_missing_evidence"
        );
    }

    #[test]
    fn a_receipt_the_public_contract_would_reject_never_escapes() {
        let mut record = completed_record(true);
        record
            .completion
            .as_mut()
            .expect("completion")
            .changed_files[0]
            .path = "../escape".into();
        assert_eq!(
            review_receipt(&record)
                .expect_err("traversal is refused")
                .reason_code(),
            "projection_invalid"
        );
    }

    #[test]
    fn status_carries_the_exact_phase_beside_the_public_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let journal = Journal::open(&dir.path().join("events.jsonl"), 8).expect("journal");
        let record = testing::run_record_fixture("run-3", RunPhase::Paused);
        let status = run_status(&record, &journal).expect("status");
        assert_eq!(status.phase, RunPhase::Paused);
        assert_eq!(
            status.durable.state,
            grokptah_agent_sdk::DurableRunState::Interrupted
        );
        assert!(!status.receipt_available);
        assert!(status.event_range.is_none());
    }

    #[test]
    fn recovery_notifications_name_the_operation_to_poll() {
        let scope = RunScope {
            session_id: "session-1".into(),
            workspace: "project".into(),
            run_id: "run-1".into(),
        };
        let value = serde_json::to_value(recovery_notification(scope, 4, "cursor_expired"))
            .expect("serializes");
        assert_eq!(value["kind"], "recovery");
        assert_eq!(value["afterSeq"], 4);
        assert_eq!(value["pollTool"], RECOVERY_POLL_OPERATION);
    }
}
