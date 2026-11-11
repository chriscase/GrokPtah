//! Durable stationarity, host-observed waits, and uncertain dispatch (#465).
//!
//! The identical-tool detector in `host_helpers` stays the classifier. This
//! module is the production policy around it: a stationary stop is a durable
//! record, a wait counts only while a host-owned witness advances under one
//! fixed deadline, and an uncertain dispatch is never an automatic retry.

use crate::orchestration::{RunRecord, RunState, RunStopCause};

/// How long one admitted wait may count as progress. The deadline is fixed at
/// admission. A later poll does not slide it.
pub const HOST_WAIT_DEADLINE_MS: i64 = 10 * 60 * 1000;

/// Stop written onto the session at the moment the host decides the loop is
/// stationary. It survives the process that decided it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStationaryStop {
    pub revision: u64,
    pub code: String,
    pub tool_name: String,
    pub run_len: u32,
    pub true_noop: bool,
}

impl SessionStationaryStop {
    pub fn stationarity(
        previous_revision: Option<u64>,
        tool_name: &str,
        run_len: u32,
        true_noop: bool,
    ) -> Self {
        Self {
            revision: previous_revision.unwrap_or(0).saturating_add(1),
            code: "stationarity".into(),
            tool_name: tool_name.to_string(),
            run_len,
            true_noop,
        }
    }
}

/// One host-admitted wait. `admitted_generation` is the host generation at the
/// last non-productive observation. Model text is not stored here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWaitWitness {
    pub task_id: String,
    pub admitted_generation: u64,
    pub deadline_unix_ms: i64,
}

/// Apply a durable stationary stop to a run that has not finished.
///
/// A terminal run is left alone. A non-terminal run becomes limit-reached with
/// `RunStopCause::Stationarity` and is not requeued.
pub fn apply_durable_stationarity_stop(run: &mut RunRecord) -> bool {
    if run.state.is_terminal() {
        return false;
    }
    run.state = RunState::LimitReached;
    run.stop_cause = Some(RunStopCause::Stationarity);
    run.terminal_result = Some("stationarity".into());
    run.error_code = Some("stationarity".into());
    true
}

/// Record one host-observed wait poll.
///
/// Returns the witness to store and whether this poll is progress. Progress
/// requires the host generation to move forward strictly before the original
/// deadline. `model_prose` is accepted and ignored: the model cannot witness
/// its own wait.
pub fn note_wait_witness(
    existing: Option<SessionWaitWitness>,
    task_id: &str,
    observed_generation: u64,
    now_unix_ms: i64,
    model_prose: &str,
) -> (SessionWaitWitness, bool) {
    let _ = model_prose;
    let mut witness = match existing {
        Some(current) if current.task_id == task_id => current,
        _ => SessionWaitWitness {
            task_id: task_id.to_string(),
            admitted_generation: observed_generation,
            deadline_unix_ms: now_unix_ms.saturating_add(HOST_WAIT_DEADLINE_MS),
        },
    };
    let productive =
        now_unix_ms < witness.deadline_unix_ms && observed_generation > witness.admitted_generation;
    if productive {
        witness.admitted_generation = observed_generation;
    }
    (witness, productive)
}

/// An uncertain dispatch is absorbing. A lower layer reporting the send as
/// safe to resend does not override that.
pub fn dispatch_may_retry_automatically(outcome_uncertain: bool, safe_to_resend: bool) -> bool {
    !outcome_uncertain && safe_to_resend
}

/// Stable output the stationarity digest sees for a wait poll. It carries the
/// host generation when the witness advanced, and a fixed token otherwise, so
/// model-shaped tool text cannot look like progress.
pub fn wait_witness_observation(task_id: &str, generation: u64, productive: bool) -> String {
    if productive {
        format!("host-witness:{task_id}:{generation}")
    } else {
        format!("host-witness:{task_id}:unchanged")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::orchestration::{RunBounds, RunPurpose};

    fn running_run() -> RunRecord {
        let now = Utc::now();
        RunRecord {
            run_id: "run-1".into(),
            session_id: Uuid::nil(),
            workspace: "/tmp/project".into(),
            request_id: "request-1".into(),
            client_id: None,
            state: RunState::Running,
            purpose: RunPurpose::Execution,
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
            prompt_preview: String::new(),
            start_seq: None,
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        }
    }

    #[test]
    fn durable_stop_limits_a_live_run_and_leaves_a_finished_one() {
        let mut live = running_run();
        assert!(apply_durable_stationarity_stop(&mut live));
        assert_eq!(live.state, RunState::LimitReached);
        assert_eq!(live.stop_cause, Some(RunStopCause::Stationarity));
        assert_eq!(live.terminal_result.as_deref(), Some("stationarity"));
        assert_eq!(live.error_code.as_deref(), Some("stationarity"));
        assert!(live.retry_of.is_none());

        let mut done = running_run();
        done.state = RunState::Completed;
        done.stop_cause = Some(RunStopCause::Completed);
        assert!(!apply_durable_stationarity_stop(&mut done));
        assert_eq!(done.state, RunState::Completed);
        assert_eq!(done.stop_cause, Some(RunStopCause::Completed));
    }

    #[test]
    fn wait_progress_requires_a_host_generation_before_a_fixed_deadline() {
        let (first, productive) = note_wait_witness(None, "task-1", 0, 1_000, "I am waiting");
        assert!(!productive);
        assert_eq!(first.deadline_unix_ms, 1_000 + HOST_WAIT_DEADLINE_MS);

        let (still, productive) = note_wait_witness(
            Some(first.clone()),
            "task-1",
            0,
            1_500,
            "still waiting, the file changed",
        );
        assert!(!productive);
        assert_eq!(still.deadline_unix_ms, first.deadline_unix_ms);
        assert_eq!(still.admitted_generation, 0);

        let (advanced, productive) = note_wait_witness(Some(still), "task-1", 1, 2_000, "done now");
        assert!(productive);
        assert_eq!(advanced.admitted_generation, 1);
        assert_eq!(advanced.deadline_unix_ms, first.deadline_unix_ms);

        let (quiet, productive) = note_wait_witness(Some(advanced.clone()), "task-1", 1, 2_100, "");
        assert!(!productive);
        assert_eq!(quiet.admitted_generation, 1);

        let after_deadline = first.deadline_unix_ms;
        let (_expired, productive) =
            note_wait_witness(Some(quiet), "task-1", 4, after_deadline, "generation moved");
        assert!(!productive);
    }

    #[test]
    fn uncertain_dispatch_is_not_an_automatic_retry() {
        assert!(!dispatch_may_retry_automatically(true, true));
        assert!(!dispatch_may_retry_automatically(true, false));
        assert!(dispatch_may_retry_automatically(false, true));
        assert!(!dispatch_may_retry_automatically(false, false));
    }
}
