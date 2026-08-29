//! In-process agent host — the shipped runtime desktop uses.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::completion::{
    build_evidence, enrich_terminal_handoff, observe_updates, CompletionObservations,
    CompletionUsage,
};
use crate::computer_agent::{
    propose_semantic_action, qualify_semantic_model, resolve_computer_eligibility,
    ComputerAgentEligibility, ComputerAgentProposal,
};
use crate::event_bus::{session_id_of, JournalPage};
use crate::events::{SessionUpdate, ToolCallKind, ToolCallStatus};
use crate::host_helpers::{
    action_stationarity_nudge, action_stationarity_stop_message, api_context_messages,
    auto_cargo_reverify_command, build_agent_messages, build_compact_summary,
    call_xai_agent_step_observed, call_xai_chat, cargo_test_failure_coaching,
    cargo_test_output_failed, cargo_test_output_passed, cargo_test_reverify_coaching,
    coding_agent_tools, count_cargo_test_failures, emit_message, emit_thought,
    filter_tools_batch_edit_only, filter_tools_edit_and_shell, filter_tools_edit_only,
    is_incomplete_stop_message, is_round_limit_stop_message, is_true_noop_tool_step,
    multi_failure_partial_edit_coaching, normalize_sandbox_profile, offline_plan_steps,
    parse_effort_arg, post_cargo_failure_skip_message, propose_plan_with_model, push_assistant,
    push_thought, push_tool, recovery_round_limit_stop_message, resolve_turn_max_rounds,
    round_limit_stop_message, sandbox_blocks_shell, sandbox_is_readonly,
    should_auto_cargo_reverify_after_edit, should_skip_tool_after_cargo_failure,
    surface_rate_limit_or_error, tool_kind, tool_step_signature, tool_web_fetch, AgentStep,
    IdenticalToolCallRun, McpToolIndex,
};
use crate::host_runtime::HostRuntime;
use crate::lane::LaneSummary;
use crate::local_tools;
use crate::memory::{MemoryAccess, MemoryAddress, MemoryScope};
use crate::orchestration::{
    apply_run_aggregate, assemble_continuation_context, prompt_preview, AgentAuthorityPolicy,
    AgentContinuationPlan, AgentLaneAssociation, AgentRecord, AgentResumePlan, AgentSpec,
    AgentState, ContinuationCheckpoint, ContinuationMemoryFact, ContinuationMemoryInput,
    ContinuationMemoryScope, ContinuationReason, ContinuationReasonCode, ContinuationRunInput,
    ContinuationTestInput, MissedRunPolicy, OrchStore, PromotionState, RoutineConcurrencyPolicy,
    RoutineLifecycle, RoutineRecord, RoutineRetryPolicy, RoutineSnapshot, RoutineTrigger,
    RunAggregates, RunBounds, RunExecution, RunExecutionMode, RunPurpose, RunRecord, RunState,
    RunStopCause, WorkAttemptView, WorkItem, WorkItemSnapshot, WorkPolicy, WorkTemplate,
    DEFAULT_AGENT_TOOL_IDS,
};
use crate::permission::{
    evaluate_tool_gate, PendingPermissionView, PermissionDecision, PermissionRequest, ToolGate,
};
use crate::prompt_queue::{
    format_interjection, PromptQueueClearOutcome, PromptQueueEntry, PromptQueueRunNextResult,
    PromptQueueSnapshot, PromptQueueTakeResult, SessionPromptQueue, SteeringDisposition,
    SteeringReceipt,
};
use crate::provider_observation::{ProviderObservationContext, ProviderObservationSession};
use crate::run_promotion::{self, RunReview};
use crate::search_engine::{self, SearchHit, SearchQuery};
use crate::session::{
    workspace_status, Session, SessionCompletion, SessionKind, SessionSummary, TranscriptEntry,
    WorkspaceStatus,
};
use crate::session_store::{self, WorkspaceChrome};
use crate::types::{
    AuthState, BackgroundTask, EffortLevel, McpProjectTrust, McpServerInfo, ModelInfo, PluginInfo,
    SkillInfo, SubagentExecutionMode, SubagentInfo, SubagentIsolationPreference,
};

/// UI restore payload: open tabs + active Lane + project.
///
/// `active_session` and `sessions` remain in the payload for older desktop
/// clients. During the compatibility migration, each Lane id equals its
/// backing session id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUiState {
    pub project_cwd: Option<String>,
    pub active_session: Option<Uuid>,
    #[serde(default)]
    pub active_lane_id: Option<Uuid>,
    pub open_tab_ids: Vec<Uuid>,
    pub model: String,
    pub effort: EffortLevel,
    pub sessions: Vec<SessionSummary>,
    #[serde(default)]
    pub lanes: Vec<LaneSummary>,
}

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub default_model: String,
    pub default_effort: EffortLevel,
    pub always_approve: bool,
    /// Cap model steps per user turn (live eval / tight budgets). None = default 24.
    pub max_agent_rounds: Option<u32>,
    /// Override the bounded event journal capacity for deterministic harnesses.
    /// Production callers leave this unset and use the standard capacity.
    pub event_bus_capacity: Option<usize>,
    /// Optional bounded structural observation of physical provider attempts.
    /// Disabled by default and never required for provider execution.
    pub provider_observation: Option<ProviderObservationSession>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            // Same source as Grok Build: config.toml [models].default, else
            // preferred id from ~/.grok/models_cache.json, else "grok-build".
            default_model: crate::models_catalog::resolve_default_model(),
            default_effort: EffortLevel::Medium,
            always_approve: false,
            max_agent_rounds: None,
            event_bus_capacity: None,
            provider_observation: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub running: bool,
    pub project_cwd: Option<String>,
    pub active_session: Option<Uuid>,
    pub always_approve: bool,
    pub model: String,
    pub effort: EffortLevel,
    pub sandbox_profile: String,
    pub appearance: String,
    pub auto_update_enabled: bool,
}

struct PendingPermission {
    /// Tool that requested this permission (for scoped AlwaysAllow).
    tool_name: String,
    session_id: Uuid,
    run_id: Option<String>,
    tx: oneshot::Sender<PermissionDecision>,
}

/// Host-global admission metadata. The prompt remains owned by the
/// orchestration service that accepted it, but scheduling order and the
/// active-turn reservation are decided under this host lock so embedded
/// control services cannot make conflicting choices.
#[derive(Debug, Clone, Copy)]
struct OrchestrationPendingAdmission {
    session_id: Uuid,
    sequence: u64,
}

#[derive(Debug, Clone)]
struct SessionComputerQualification {
    route_fingerprint: String,
}

pub(crate) struct Inner {
    running: bool,
    project_cwd: Option<PathBuf>,
    pub(crate) sessions: HashMap<Uuid, Session>,
    active_session: Option<Uuid>,
    /// Tab strip order from the last desktop session (persisted).
    open_tab_ids: Vec<Uuid>,
    always_approve: bool,
    always_allowed_tools: HashSet<String>,
    /// Optional per-turn model-step budget (#187/#188).
    max_agent_rounds: Option<u32>,
    model: String,
    effort: EffortLevel,
    auth: AuthState,
    sandbox_profile: String,
    subagent_isolation: SubagentIsolationPreference,
    appearance: String,
    permission_mode: String,
    allow_rules: Vec<String>,
    deny_rules: Vec<String>,
    mcp_servers: Vec<McpServerInfo>,
    plugins: Vec<PluginInfo>,
    skills: Vec<SkillInfo>,
    subagents: Vec<SubagentInfo>,
    /// Per-subagent cancel tokens (#151/#152) — cancel one child without killing siblings.
    subagent_cancels: HashMap<String, CancellationToken>,
    background_tasks: Vec<BackgroundTask>,
    /// Cancel tokens for in-flight background tasks (#52).
    background_cancels: HashMap<String, CancellationToken>,
    pending_permissions: HashMap<Uuid, PendingPermission>,
    /// Per-session turn cancellation so multiple sessions can run concurrently
    /// (Claude Code–style parallel build sessions).
    turn_cancels: HashMap<Uuid, CancellationToken>,
    /// Identity of the turn currently installed in `turn_cancels`, so a caller
    /// that observed a turn under the lock can prove the turn it is about to
    /// cancel is still that same turn. Without it, a turn that finishes while
    /// the lock is released lets the next turn absorb someone else's cancel.
    turn_generations: HashMap<Uuid, u64>,
    /// Monotonic across all sessions; a generation is never reused.
    next_turn_generation: u64,
    /// Explicit, short-lived model qualification/proposal calls from the
    /// Computer cockpit. These are independent from Build turns and always
    /// cancelled by local Stop/Take over.
    computer_agent_operations: HashMap<Uuid, (String, CancellationToken)>,
    /// Session-local measured authority for built-in/provider routes that do
    /// not have a durable provider-profile capability record. Restart clears it.
    computer_agent_qualifications: HashMap<(Uuid, String), SessionComputerQualification>,
    /// Short-lived orchestration admission reservations. These close the gap
    /// between accepting a run and polling its async prompt future.
    turn_reservations: HashMap<Uuid, String>,
    /// When a queue-drain reservation was taken. A drain claims the turn slot
    /// and hands it to a separate start call, so a caller that dies in between
    /// would otherwise wedge the session as permanently busy. Only drain
    /// reservations are reclaimable, and only after [`DRAIN_RESERVATION_TTL`].
    drain_reservations: HashMap<Uuid, std::time::Instant>,
    /// Host-global orchestration admissions shared by every control service.
    orchestration_admissions: HashMap<String, Uuid>,
    /// Host-global bounded pending admissions shared by every control service.
    orchestration_pending_admissions: HashMap<String, OrchestrationPendingAdmission>,
    /// Monotonic arrival order for queued task admissions.
    orchestration_next_pending_sequence: u64,
    /// Last session selected by the host-global fair scheduler.
    orchestration_last_started_session: Option<Uuid>,
    /// One authoritative ceiling shared by every control service on this host.
    orchestration_admission_limit: usize,
    /// Authoritative follow-up queue plus non-cancelling steering inbox.
    prompt_queues: HashMap<Uuid, SessionPromptQueue>,
    /// Per-session commit sequence for [`Inner::prompt_queues`], bumped under
    /// this lock by every mutation. `PromptQueueChanged` carries it so
    /// consumers can discard snapshots that were published out of commit
    /// order (publishing happens after the lock is released).
    prompt_queue_revisions: HashMap<Uuid, u64>,
    /// Per-turn model-step budget override (orchestration `RunBounds.max_rounds`).
    turn_max_rounds: HashMap<Uuid, u32>,
    event_tx: crate::event_bus::EventBus,
    /// Paths the agent wrote/edited this process (for diff review).
    edited_files: Vec<String>,
    /// Per-session path → original content before first agent edit (#146).
    /// Keyed by session so rewind never restores another session's edits.
    edit_snapshots: HashMap<Uuid, HashMap<String, String>>,
    /// Live tool shell child — killed by [`AgentHostHandle::cancel_turn`].
    live_shells: local_tools::LiveShellMap,
    /// Session usage counters (#159) — prompt/completion tokens when API reports them.
    pub(crate) session_usage: HashMap<Uuid, SessionUsage>,
}

impl Inner {
    /// Stamp the next commit sequence for `session_id`'s prompt queue.
    ///
    /// Must be called while still holding the lock that performed the
    /// mutation: the returned value is what orders the resulting
    /// `PromptQueueChanged` against concurrent mutations, and events are
    /// published after the lock is dropped.
    fn next_queue_revision(&mut self, session_id: Uuid) -> u64 {
        let slot = self.prompt_queue_revisions.entry(session_id).or_insert(0);
        *slot += 1;
        *slot
    }

    /// Revision a reader should stamp on a snapshot it just read: the newest
    /// one already committed, without claiming a new one.
    fn current_queue_revision(&self, session_id: Uuid) -> u64 {
        self.prompt_queue_revisions
            .get(&session_id)
            .copied()
            .unwrap_or(0)
    }

    /// Drop a queue-drain reservation whose start call never arrived, so a
    /// dead drainer cannot leave the session busy forever. Returns true if one
    /// was reclaimed.
    fn reclaim_expired_drain_reservation(&mut self, session_id: Uuid) -> bool {
        let expired = self
            .drain_reservations
            .get(&session_id)
            .is_some_and(|taken| taken.elapsed() >= DRAIN_RESERVATION_TTL);
        if expired {
            self.drain_reservations.remove(&session_id);
            self.turn_reservations.remove(&session_id);
        }
        expired
    }

    /// Install a fresh turn identity. Called under the same lock that inserts
    /// into `turn_cancels`, so observers see the pair atomically.
    fn begin_turn_generation(&mut self, session_id: Uuid) -> u64 {
        self.next_turn_generation += 1;
        let generation = self.next_turn_generation;
        self.turn_generations.insert(session_id, generation);
        generation
    }
}

struct PromptQueueRecovery {
    entries: Vec<PromptQueueEntry>,
    revision: u64,
}

/// What a recovery attempt actually achieved.
enum PromptQueueRecoveryOutcome {
    /// Nothing was pending; the queue is untouched.
    Nothing,
    /// Recovered and durably committed. Safe to publish as authoritative.
    Committed(PromptQueueRecovery),
    /// Recovered in memory, but the durable write failed.
    ///
    /// The steering is still in the live queue — dropping it here would lose an
    /// interjection the operator already accepted, and the entry that a
    /// caller can no longer see is the worst of the available outcomes. What
    /// is withheld is the *claim of authority*: no queue snapshot is
    /// published, because publishing one would assert a durable commit that
    /// did not happen. The failure is reported instead.
    NotPersisted { error: anyhow::Error },
}

/// Recover steering into the durable queue and capture the committed snapshot.
///
/// The caller must hold the `Inner` mutation lock.
///
/// Ordering is persist-then-publish: the durable write is attempted before any
/// snapshot is published, so a failed save can never produce an event for a
/// mutation that was not committed. A failed save does **not** discard the
/// recovery, though. The recovery is applied to the live queue either way and
/// the caller is handed the error to report, because the previous behaviour —
/// return `Err` before touching the queue — left accepted steering stranded in
/// `steering`/`delivering`, where no later boundary would deliver it and
/// neither the GUI nor `ptah_get_queue` could see it.
/// `write` is `None` when this runtime no longer owns durable writes for its
/// home — a turn tearing down during shutdown, or a stale handle. The recovery
/// is still applied in memory (dropping it would lose the interjection
/// outright), but nothing is persisted and the caller is handed the same
/// `NotPersisted` outcome an IO failure produces, so no revision is claimed for
/// a mutation that will not survive a restart (#455).
fn recover_pending_steering_locked(
    write: Option<&crate::host_runtime::DurableWriteGuard>,
    g: &mut Inner,
    session_id: Uuid,
) -> PromptQueueRecoveryOutcome {
    let mut next = g
        .prompt_queues
        .get(&session_id)
        .cloned()
        .unwrap_or_default();
    if next.recover_pending_steering() == 0 {
        return PromptQueueRecoveryOutcome::Nothing;
    }

    let entries = next.list();
    let persisted = match write {
        Some(write) => session_store::save_prompt_queue(write, session_id, &next)
            .map_err(|error| anyhow!("persist steering recovery: {error}")),
        None => Err(anyhow!(
            "persist steering recovery: this process no longer holds durable-write \
             authority for its GrokPtah home"
        )),
    };
    // Applied regardless: the in-memory queue is what the session actually
    // runs from, and leaving it un-recovered loses the interjection outright.
    g.prompt_queues.insert(session_id, next);
    match persisted {
        Ok(()) => {
            let revision = g.next_queue_revision(session_id);
            PromptQueueRecoveryOutcome::Committed(PromptQueueRecovery { entries, revision })
        }
        // No revision is claimed for an uncommitted mutation, so consumers
        // holding a watermark are not advanced past a state that may not
        // survive a restart.
        Err(error) => PromptQueueRecoveryOutcome::NotPersisted { error },
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunTokenStop {
    Reached { consumed: u64, ceiling: u64 },
    UsageUnavailable { ceiling: u64 },
    AccountingOverflow { ceiling: u64 },
}

impl RunTokenStop {
    fn code(self) -> &'static str {
        match self {
            Self::Reached { .. } => "max_total_tokens_reached",
            Self::UsageUnavailable { .. } => "max_total_tokens_usage_unavailable",
            Self::AccountingOverflow { .. } => "max_total_tokens_accounting_overflow",
        }
    }

    fn cause(self) -> RunStopCause {
        match self {
            Self::Reached { .. } => RunStopCause::TokenCeiling,
            Self::UsageUnavailable { .. } => RunStopCause::TokenAccountingUnavailable,
            Self::AccountingOverflow { .. } => RunStopCause::TokenAccountingOverflow,
        }
    }

    fn message(self) -> String {
        match self {
            Self::Reached { consumed, ceiling } => format!(
                "Stopped at the run token boundary: consumed {consumed} total tokens, meeting or exceeding the max_total_tokens ceiling of {ceiling}."
            ),
            Self::UsageUnavailable { ceiling } => format!(
                "Stopped at the run token boundary because the provider did not return usable token metadata for a run bounded by max_total_tokens={ceiling}."
            ),
            Self::AccountingOverflow { ceiling } => format!(
                "Stopped at the run token boundary because token accounting overflowed for a run bounded by max_total_tokens={ceiling}."
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct RunUsageState {
    usage: CompletionUsage,
    complete: bool,
    pending_requests: u32,
    stop: Option<RunTokenStop>,
}

struct RunUsageTracker {
    run_id: String,
    store: OrchStore,
    max_total_tokens: Option<u64>,
    state: Mutex<RunUsageState>,
    bounded_admission: Arc<tokio::sync::Mutex<()>>,
}

struct RunUsageAttempt {
    tracker: Arc<RunUsageTracker>,
    _bounded_admission: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl RunUsageAttempt {
    fn finish(self, usage: Option<&CompletionUsage>) -> Result<Option<String>> {
        self.tracker.finish_attempt(usage)
    }
}

impl RunUsageTracker {
    fn from_run(store: OrchStore, run: &RunRecord) -> Arc<Self> {
        Arc::new(Self {
            run_id: run.run_id.clone(),
            store,
            max_total_tokens: run.bounds.max_total_tokens,
            state: Mutex::new(RunUsageState {
                usage: run.aggregates.usage.clone(),
                complete: run.aggregates.usage_complete,
                pending_requests: run.aggregates.usage_pending_requests,
                stop: None,
            }),
            bounded_admission: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    fn stop_message(&self) -> Option<String> {
        self.state.lock().stop.map(RunTokenStop::message)
    }

    fn run_id(&self) -> &str {
        &self.run_id
    }

    #[cfg(test)]
    fn stop_code(&self) -> Option<&'static str> {
        self.state.lock().stop.map(RunTokenStop::code)
    }

    fn durable_stop_code(&self) -> Option<String> {
        self.store
            .load_run(&self.run_id)
            .ok()
            .flatten()
            .and_then(|run| run.stop_cause.map(|_| run.error_code))
            .flatten()
    }

    fn mark_host_stop(&self, cause: RunStopCause, code: &str) -> Result<()> {
        self.store
            .update_run(&self.run_id, |run| {
                run.error_code = Some(code.into());
                run.stop_cause = Some(cause);
                run.updated_at = Utc::now();
                Ok(())
            })?
            .ok_or_else(|| anyhow!("run disappeared while recording its stop cause"))?;
        Ok(())
    }

    fn is_bounded(&self) -> bool {
        self.max_total_tokens.is_some()
    }

    async fn begin_attempt(self: &Arc<Self>) -> Result<RunUsageAttempt> {
        let bounded_admission = if self.is_bounded() {
            Some(self.bounded_admission.clone().lock_owned().await)
        } else {
            None
        };
        if let Some(stop) = self.state.lock().stop {
            bail!(stop.message());
        }
        let pending_requests = {
            let mut state = self.state.lock();
            state.pending_requests = state
                .pending_requests
                .checked_add(1)
                .ok_or_else(|| anyhow!("provider attempt counter overflowed"))?;
            state.pending_requests
        };
        match self.store.update_run(&self.run_id, |run| {
            run.aggregates.usage_pending_requests = pending_requests;
            run.updated_at = Utc::now();
            Ok(())
        }) {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.state.lock().pending_requests = pending_requests.saturating_sub(1);
                bail!("run disappeared while admitting a provider request");
            }
            Err(error) => {
                self.state.lock().pending_requests = pending_requests.saturating_sub(1);
                return Err(error);
            }
        }
        Ok(RunUsageAttempt {
            tracker: self.clone(),
            _bounded_admission: bounded_admission,
        })
    }

    fn finish_attempt(&self, usage: Option<&CompletionUsage>) -> Result<Option<String>> {
        {
            let mut state = self.state.lock();
            state.pending_requests = state.pending_requests.saturating_sub(1);
        }
        self.record(usage)
    }

    fn record(&self, usage: Option<&CompletionUsage>) -> Result<Option<String>> {
        let (snapshot, complete, pending_requests, stop) = {
            let mut state = self.state.lock();
            match usage {
                Some(usage) => {
                    let next = (|| {
                        Some(CompletionUsage {
                            prompt_tokens: state
                                .usage
                                .prompt_tokens
                                .checked_add(usage.prompt_tokens)?,
                            completion_tokens: state
                                .usage
                                .completion_tokens
                                .checked_add(usage.completion_tokens)?,
                            total_tokens: state
                                .usage
                                .total_tokens
                                .checked_add(usage.total_tokens)?,
                            requests: state.usage.requests.checked_add(usage.requests)?,
                        })
                    })();
                    if let Some(next) = next {
                        state.usage = next;
                        if let Some(ceiling) = self.max_total_tokens {
                            if state.usage.total_tokens >= ceiling {
                                state.stop = Some(RunTokenStop::Reached {
                                    consumed: state.usage.total_tokens,
                                    ceiling,
                                });
                            }
                        }
                    } else if let Some(ceiling) = self.max_total_tokens {
                        state.complete = false;
                        state.stop = Some(RunTokenStop::AccountingOverflow { ceiling });
                    } else {
                        bail!("run token accounting overflowed");
                    }
                }
                None => {
                    state.complete = false;
                    if let Some(ceiling) = self.max_total_tokens {
                        state.stop = Some(RunTokenStop::UsageUnavailable { ceiling });
                    }
                }
            }
            (
                state.usage.clone(),
                state.complete,
                state.pending_requests,
                state.stop,
            )
        };
        self.store
            .update_run(&self.run_id, |run| {
                run.aggregates.usage = snapshot.clone();
                // Once terminal teardown or restart has declared an attempt
                // unresolved, a late response may add measured totals but may
                // not restore the stronger claim that accounting is complete.
                run.aggregates.usage_complete &= complete;
                run.aggregates.usage_pending_requests = pending_requests;
                if let Some(verification) = run.aggregates.verification.as_mut() {
                    verification.usage = snapshot.clone();
                }
                if let Some(stop) = stop {
                    if run.stop_cause != Some(RunStopCause::TokenAccountingUnavailable) {
                        run.error_code = Some(stop.code().into());
                        run.stop_cause = Some(stop.cause());
                    }
                }
                run.updated_at = Utc::now();
                Ok(())
            })?
            .ok_or_else(|| anyhow!("run disappeared while recording provider usage"))?;
        Ok(stop.map(RunTokenStop::message))
    }
}

const MAX_SESSION_COMPLETION_HISTORY: usize = 64;

/// How long a queue-drain reservation may go unclaimed before another drain
/// may take the slot. A drain that has not started its turn within this window
/// is not coming back, and the session must not stay busy on its behalf.
const DRAIN_RESERVATION_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Clears `turn_cancels` for a session when dropped — keeps panics from wedging busy.
struct TurnBusyGuard {
    host: AgentHostHandle,
    session_id: Uuid,
    armed: bool,
}

struct ComputerAgentBusyGuard {
    host: AgentHostHandle,
    session_id: Uuid,
    operation_id: String,
}

impl Drop for TurnBusyGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let write = self.host.durable_write("recovering pending steering").ok();
        let outcome = {
            let mut g = self.host.inner.lock();
            g.turn_cancels.remove(&self.session_id);
            g.turn_generations.remove(&self.session_id);
            g.turn_max_rounds.remove(&self.session_id);
            recover_pending_steering_locked(write.as_ref(), &mut g, self.session_id)
        };
        match outcome {
            PromptQueueRecoveryOutcome::Nothing => {}
            PromptQueueRecoveryOutcome::Committed(recovery) => {
                self.host
                    .emit_pending_steering_recovery(self.session_id, recovery);
                // Already durable; a second write here would only add
                // contention on the session's temp file.
                return;
            }
            // This path used to discard the error entirely, so an abort-path
            // persistence failure was invisible — unlike the agent-error path,
            // which reports it. Same failure, same contract, both audible now.
            PromptQueueRecoveryOutcome::NotPersisted { error } => {
                self.host
                    .report_steering_recovery_failure(self.session_id, &error);
            }
        }
        let _ = self.host.persist_prompt_queue(self.session_id);
    }
}

impl Drop for ComputerAgentBusyGuard {
    fn drop(&mut self) {
        let mut inner = self.host.inner.lock();
        if inner
            .computer_agent_operations
            .get(&self.session_id)
            .is_some_and(|(operation_id, _)| operation_id == &self.operation_id)
        {
            inner.computer_agent_operations.remove(&self.session_id);
        }
    }
}

async fn kill_shells(live_shells: local_tools::LiveShellMap, kill_ids: Vec<Uuid>) {
    let mut map = live_shells.lock().await;
    for id in kill_ids {
        if let Some(mut child) = map.remove(&id) {
            crate::process_tree::terminate(&mut child).await;
        }
    }
}

fn canonical_session_workspace(
    host: &AgentHostHandle,
    session_id: Uuid,
    recorded_source: &str,
) -> Result<PathBuf> {
    let session_cwd = host
        .inner
        .lock()
        .sessions
        .get(&session_id)
        .map(|session| session.cwd.clone())
        .ok_or_else(|| anyhow!("unknown session"))?;
    let source = dunce::canonicalize(recorded_source)
        .with_context(|| format!("canonicalize source workspace {recorded_source}"))?;
    let session_cwd = dunce::canonicalize(session_cwd).context("canonicalize session workspace")?;
    if source != session_cwd {
        bail!("run source workspace no longer matches the session workspace");
    }
    Ok(source)
}

fn validate_run_approval(
    run: &RunRecord,
    approval: &crate::orchestration::RunApproval,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    let execution = run
        .execution
        .as_ref()
        .ok_or_else(|| anyhow!("run has no isolated execution"))?;
    if approval.run_id != run.run_id
        || approval.session_id != run.session_id
        || approval.workspace != run.workspace
        || approval.source_fingerprint != execution.source_fingerprint
        || approval.final_fingerprint != execution.final_fingerprint.as_deref().unwrap_or_default()
    {
        bail!("approval scope does not match the current run");
    }
    if now >= approval.expires_at {
        bail!("approval has expired");
    }
    Ok(())
}

/// Shared handle used by Tauri state and tests.
#[derive(Clone)]
pub struct AgentHostHandle {
    pub(crate) inner: Arc<Mutex<Inner>>,
    event_rx_factory: Arc<Mutex<Option<crate::event_bus::EventReceiver>>>,
    /// One process-owned durable run ledger shared by desktop and MCP.
    /// It is opened lazily so library users can still construct a host for
    /// tests that provide their own orchestration store.
    orchestration_store: Arc<Mutex<Option<OrchStore>>>,
    /// One process-owned durable Computer Run ledger (#271). The store holds
    /// an exclusive file lock, so the desktop cockpit and the MCP control
    /// plane must share this handle instead of opening their own.
    computer_store: Arc<Mutex<Option<crate::computer_use::ComputerStore>>>,
    /// Prevent concurrent desktop promotion/discard operations for one run.
    promotion_locks: Arc<Mutex<HashSet<String>>>,
    reviewed_runs: Arc<Mutex<HashSet<String>>>,
    /// Wakes every embedded orchestration service after a global admission
    /// slot is actually released, after the completion event itself.
    orchestration_wakeup: Arc<Notify>,
    /// Explicit run-scoped accounting shared by the parent model loop and any
    /// children it spawns. A session counter is not a safe run identity.
    run_usage_trackers: Arc<Mutex<HashMap<Uuid, Arc<RunUsageTracker>>>>,
    provider_observation: Option<ProviderObservationSession>,
    /// Shared process lifecycle (#455). The handle observes the phase and
    /// registers supervised tasks; it deliberately does **not** own the
    /// single-instance lock, which lives in the non-cloneable
    /// [`crate::HostRuntime`] so release is an explicit ordered action rather
    /// than a clone-refcount side effect.
    pub(crate) lifecycle: Arc<crate::host_runtime::HostLifecycle>,
    /// Selects the durable root for legacy modules that still resolve paths
    /// through `grokptah_home()`. Shared by all host clones.
    runtime_home: crate::discover::RuntimeHome,
    _runtime_home_context: Arc<crate::discover::RuntimeHomeContext>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExternalRunContext {
    pub run_id: String,
    pub execution_mode: RunExecutionMode,
}

pub struct AgentHost;

impl AgentHost {
    /// Create a new host runtime. Events are pulled via
    /// [`AgentHostHandle::take_event_receiver`] once.
    ///
    /// The returned [`HostRuntime`] is **not** `Clone`: it is the single owner
    /// of the process instance lock and of the task supervisor (#455). It
    /// derefs to [`AgentHostHandle`], and `runtime.clone()` yields a cloneable
    /// *request handle* that carries no process authority of its own.
    pub fn create(config: HostConfig) -> Result<HostRuntime> {
        Self::create_with_runtime_home(config, crate::discover::RuntimeHome::discover())
    }

    /// Create a host against an explicit, validated durable runtime home.
    /// Desktop callers may keep using [`Self::create`]; hosted callers should
    /// inject their configured home so state selection is not implicit.
    pub fn create_with_runtime_home(
        config: HostConfig,
        runtime_home: crate::discover::RuntimeHome,
    ) -> Result<HostRuntime> {
        let runtime_home_context = Arc::new(runtime_home.install());
        // Exclusive ownership is the *precondition* for construction, not a
        // warning on the way past it (#455). Nothing writable is initialized
        // before this: no keychain read, no workspace load or migration, no
        // session GC, no event journal, no durable store. A host that could not
        // take the lock must not exist at all — a half-constructed one used to
        // go on to touch every one of those surfaces on a home another process
        // owns.
        let instance_lock = crate::instance_lock::InstanceLock::try_acquire_at(&runtime_home)
            .with_context(|| {
                format!(
                    "acquire the GrokPtah single-instance lock for {}",
                    runtime_home.path().display()
                )
            })?;
        let lifecycle = crate::host_runtime::HostLifecycle::new(
            Some(instance_lock),
            runtime_home.instance_lock_path(),
        );
        let mut event_tx = crate::event_bus::EventBus::new(
            config
                .event_bus_capacity
                .unwrap_or(crate::event_bus::DEFAULT_JOURNAL_CAPACITY),
        );
        {
            event_tx = event_tx.with_persist_dir(runtime_home.orchestration_root());
            // Bind the journal's authority explicitly to *this* lifecycle rather
            // than leaving it resolved by home lookup. The bind verifies the
            // journal's canonical home is the one this runtime holds the lock
            // for, so the journal fails closed with this runtime and can never
            // borrow its authority for a different home (#455).
            //
            // The journal is under this runtime's home by construction, so this
            // always binds; a false here would mean the home moved underneath us.
            debug_assert!(
                event_tx.bind_journal_lifecycle(&lifecycle),
                "the event journal must live in the home this runtime owns"
            );
        }
        // Keep a dedicated channel for take_event_receiver / first GUI subscriber.
        let event_rx = event_tx.subscribe();
        let auth = crate::auth_store::load_auth_state();
        // Construction takes ordinary counted authority: the lifecycle was
        // just created `Running` with the lock in hand, so this cannot fail,
        // and being counted means it is not a special case the seal ignores.
        let startup_write = lifecycle
            .begin_durable_write("initializing the host from its durable home")
            .context("durable-write authority for host construction")?;
        let (chrome, mut sessions) =
            session_store::load_workspace(&startup_write).unwrap_or_else(|e| {
                eprintln!("[grokptah] workspace load failed: {e:#}");
                (WorkspaceChrome::default(), HashMap::new())
            });
        let project_cwd = session_store::cwd_still_valid(chrome.project_cwd.as_deref());
        let mcp_servers = crate::discover::load_mcp_servers(project_cwd.as_deref());
        let plugins = crate::discover::discover_plugins();
        let skills = crate::discover::discover_skills(project_cwd.as_deref());
        // Prefer persisted model; fall back to HostConfig / catalog default.
        let mut model = if !chrome.model.is_empty() {
            chrome.model.clone()
        } else {
            config.default_model.clone()
        };
        // The v1 gateway applied one corporate base to the global model. Keep
        // that user's route on upgrade, but encode the provider identity so
        // credentials and endpoints can no longer be selected independently.
        let provider_config = crate::gateway_config::load();
        let legacy_or_env_route = !provider_config.base_url.trim().is_empty()
            || provider_config
                .active_profile_id
                .as_deref()
                .and_then(|id| provider_config.profile(id))
                .is_some_and(|profile| profile.managed_by_env);
        if legacy_or_env_route && !model.starts_with(crate::gateway_config::MODEL_SELECTION_PREFIX)
        {
            if let Some(profile_id) = provider_config.active_profile_id.as_deref() {
                model = crate::gateway_config::model_selection_key(profile_id, &model);
            }
        }
        let mut effort = chrome.effort;
        if let Ok(selection) = crate::gateway_config::parse_model_selection(&model) {
            let supports_current_effort =
                crate::gateway_config::resolve_profile_for_selection(&selection, false, None)
                    .is_ok_and(|profile| {
                        profile.accepts_effort(&selection.model_id, effort.as_str())
                    });
            if !supports_current_effort {
                effort = EffortLevel::None;
            }
        }
        let mut open_tab_ids = chrome.open_tab_ids.clone();
        // Drop tab ids that no longer exist.
        open_tab_ids.retain(|id| sessions.contains_key(id));
        // Construction holds the instance lock, so GC can never touch another
        // process's sessions.
        if let Ok(n) = session_store::garbage_collect(&startup_write, &open_tab_ids, 80, 24 * 7) {
            if n > 0 {
                if let Ok(reloaded) = session_store::load_all_metas() {
                    sessions = reloaded;
                    open_tab_ids.retain(|id| sessions.contains_key(id));
                }
            }
        }
        let active_session = chrome
            .active_session
            .filter(|id| sessions.get(id).is_some_and(|session| !session.archived))
            .or_else(|| {
                open_tab_ids
                    .iter()
                    .find(|id| sessions.get(id).is_some_and(|session| !session.archived))
                    .copied()
            })
            .or_else(|| {
                sessions
                    .iter()
                    .find_map(|(id, session)| (!session.archived).then_some(*id))
            });
        let prompt_queues = session_store::load_all_prompt_queues(sessions.keys().copied());
        let inner = Inner {
            running: false,
            project_cwd,
            sessions,
            active_session,
            open_tab_ids,
            always_approve: chrome.always_approve || config.always_approve,
            always_allowed_tools: HashSet::new(),
            max_agent_rounds: config.max_agent_rounds,
            model,
            effort,
            auth,
            sandbox_profile: if chrome.sandbox_profile.is_empty() {
                "workspace-write".into()
            } else {
                chrome.sandbox_profile
            },
            subagent_isolation: chrome.subagent_isolation,
            appearance: if chrome.appearance.is_empty() {
                "dark".into()
            } else {
                chrome.appearance
            },
            permission_mode: "default".into(),
            allow_rules: Vec::new(),
            deny_rules: Vec::new(),
            mcp_servers,
            plugins,
            skills,
            subagents: Vec::new(),
            subagent_cancels: HashMap::new(),
            background_tasks: Vec::new(),
            background_cancels: HashMap::new(),
            pending_permissions: HashMap::new(),
            turn_cancels: HashMap::new(),
            turn_generations: HashMap::new(),
            next_turn_generation: 0,
            computer_agent_operations: HashMap::new(),
            computer_agent_qualifications: HashMap::new(),
            turn_reservations: HashMap::new(),
            drain_reservations: HashMap::new(),
            orchestration_admissions: HashMap::new(),
            orchestration_pending_admissions: HashMap::new(),
            orchestration_next_pending_sequence: 0,
            orchestration_last_started_session: None,
            orchestration_admission_limit: usize::MAX,
            prompt_queues,
            prompt_queue_revisions: HashMap::new(),
            turn_max_rounds: HashMap::new(),
            event_tx,
            edited_files: Vec::new(),
            edit_snapshots: HashMap::new(), // session_id → path → original
            live_shells: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            session_usage: HashMap::new(),
        };
        let handle = AgentHostHandle {
            inner: Arc::new(Mutex::new(inner)),
            event_rx_factory: Arc::new(Mutex::new(Some(event_rx))),
            orchestration_store: Arc::new(Mutex::new(None)),
            computer_store: Arc::new(Mutex::new(None)),
            promotion_locks: Arc::new(Mutex::new(HashSet::new())),
            reviewed_runs: Arc::new(Mutex::new(HashSet::new())),
            orchestration_wakeup: Arc::new(Notify::new()),
            run_usage_trackers: Arc::new(Mutex::new(HashMap::new())),
            provider_observation: config.provider_observation,
            lifecycle: lifecycle.clone(),
            runtime_home,
            _runtime_home_context: runtime_home_context,
        };
        Ok(HostRuntime::new(handle, lifecycle))
    }
}

impl AgentHostHandle {
    /// The validated durable root owned by this host process.
    pub fn runtime_home(&self) -> crate::discover::RuntimeHome {
        self.runtime_home.clone()
    }

    /// Current lifecycle phase of the owning [`crate::HostRuntime`] (#455).
    pub fn lifecycle_phase(&self) -> crate::host_runtime::HostPhase {
        self.lifecycle.phase()
    }

    /// True while this handle may still take new process authority. A handle
    /// that outlived its runtime reports false and refuses new work.
    pub fn is_accepting_work(&self) -> bool {
        self.lifecycle.is_open()
    }

    /// Whether this handle can still perform durable writes at all. False once
    /// the owning runtime has sealed or closed, which is the signal a bounded
    /// retry uses to stop rather than spin (#455).
    pub fn can_write_durably(&self) -> bool {
        !self.lifecycle.durable_writes_sealed()
            && self.lifecycle.phase() != crate::host_runtime::HostPhase::Closed
    }

    /// Fail-closed guard for authority-bearing operations.
    pub(crate) fn ensure_accepting(&self, operation: &str) -> Result<()> {
        self.lifecycle.ensure_open(operation)
    }

    /// Refuse an embedder-owned effect once ordered shutdown has begun.
    ///
    /// Async effects should normally use [`Self::track_supervised`] so the
    /// runtime also joins them. This narrow synchronous gate is for local
    /// state transitions that cannot be represented as a future (for example,
    /// discarding a pending desktop approval). It grants no durable-write
    /// authority and cannot reopen a quiescing runtime.
    pub fn ensure_effect_allowed(&self, operation: &str) -> Result<()> {
        self.lifecycle.ensure_open(operation)
    }

    /// A token that can mint durable-write authority later, for operations
    /// that do slow work before their write (#455). Holding a guard across
    /// network I/O would let an ordinary slow request block the shutdown seal.
    pub(crate) fn write_authority(&self) -> crate::host_runtime::WriteAuthority {
        crate::host_runtime::WriteAuthority::new(self.lifecycle.clone())
    }

    /// Mint durable-write authority for this home, or fail closed (#455).
    ///
    /// Every durable mutator in this crate takes the returned guard by
    /// reference, so a stale handle cannot reach one: the compiler, not a
    /// reviewer, is what keeps the write behind the lifecycle check. Ordered
    /// shutdown seals this authority *before* releasing the process lock, so a
    /// replacement process can never write the same home concurrently.
    pub(crate) fn durable_write(
        &self,
        operation: &str,
    ) -> Result<crate::host_runtime::DurableWriteGuard> {
        self.lifecycle.begin_durable_write(operation)
    }

    /// Test seam: hold durable-write authority open, so a test can observe what
    /// shutdown and `Drop` do while a writer is genuinely in flight (#455).
    ///
    /// This is the *same* authority every production durable write takes; it is
    /// exposed so tests can reproduce a concurrent writer rather than simulate
    /// one.
    pub fn hold_durable_write_for_test(
        &self,
        operation: &str,
    ) -> Result<crate::host_runtime::DurableWriteLease> {
        self.durable_write(operation)
            .map(crate::host_runtime::DurableWriteLease::new)
    }

    /// Track a future on the shutdown join barrier without spawning it, for
    /// embedders that own their executor (see [`crate::HostRuntime::track`]).
    pub fn track_supervised<F>(
        &self,
        operation: &str,
        future: F,
    ) -> Result<tokio_util::task::task_tracker::TrackedFuture<F>>
    where
        F: std::future::Future,
    {
        self.lifecycle.track_future(operation, future)
    }

    /// Cancellation token fired when the owning runtime begins shutdown.
    /// Public so embedders and lifecycle tests can observe the same signal
    /// supervised tasks select on.
    pub fn shutdown_signal(&self) -> CancellationToken {
        self.lifecycle.cancel_token()
    }

    /// Test seam: bind a store to this runtime, so a test can prove a ledger
    /// for another home is refused (#455).
    pub fn bind_store_for_test(&self, store: &OrchStore) -> bool {
        store.bind_lifecycle(&self.lifecycle)
    }

    /// Cancellation token fired when the owning runtime begins shutdown.
    /// Long-lived supervised tasks select on this so the join barrier is
    /// bounded without polling or sleeping.
    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.lifecycle.cancel_token()
    }

    /// Spawn a task the owning runtime must join before releasing the process
    /// lock. Refused once shutdown has begun (#455).
    pub fn spawn_supervised<F>(
        &self,
        operation: &str,
        future: F,
    ) -> Result<tokio::task::JoinHandle<F::Output>>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.lifecycle.spawn_supervised(operation, future)
    }

    /// Cancel every unit of in-flight work this host owns, so the shutdown
    /// join barrier is bounded: turns (which also cascade to their subagents
    /// and tool shells), standalone subagents, background scans and shell
    /// tasks, Computer Use operations, and any live child processes.
    ///
    /// Cancellation is cooperative — durable finalization still runs — and the
    /// caller joins the supervised tasks afterwards.
    pub async fn cancel_all_activity(&self) {
        let (live_shells, shell_ids) = {
            let mut g = self.inner.lock();
            for token in g.turn_cancels.values() {
                token.cancel();
            }
            for (_, token) in g.subagent_cancels.drain() {
                token.cancel();
            }
            for subagent in g.subagents.iter_mut() {
                if subagent.status == "running" {
                    subagent.status = "cancelled".into();
                    subagent.summary = Some("host shutdown".into());
                }
            }
            for (_, token) in g.background_cancels.drain() {
                token.cancel();
            }
            for task in g.background_tasks.iter_mut() {
                if task.status == "running" {
                    task.status = "cancelled".into();
                    task.detail = Some("host shutdown".into());
                }
            }
            for (_, (_, token)) in g.computer_agent_operations.drain() {
                token.cancel();
            }
            let shell_ids: Vec<Uuid> = g.turn_cancels.keys().copied().collect();
            (g.live_shells.clone(), shell_ids)
        };
        kill_shells(live_shells, shell_ids).await;
        // Any surviving child process for a session with no live turn.
        let orphans: Vec<Uuid> = {
            let map = self.inner.lock().live_shells.clone();
            let guard = map.lock().await;
            guard.keys().copied().collect()
        };
        if !orphans.is_empty() {
            let map = self.inner.lock().live_shells.clone();
            kill_shells(map, orphans).await;
        }
        self.invalidate_computer_agent_authority();
    }

    /// Persist the durable state this process owns and release the shared
    /// ledgers, so a replacement host on the same home reopens a consistent
    /// world. Called by ordered shutdown after every supervised task joined.
    ///
    /// Returns one stable description per failure. A caller that reports a
    /// clean lock release while this returned errors would be lying about the
    /// durable state it left behind, so the report carries them (#455).
    pub fn flush_durable_state(&self) -> Vec<String> {
        let mut errors = Vec::new();
        // The flush is this runtime's own last write, and it runs after the
        // durable-write seal. `flush_write` is the one authority that outlives
        // the seal, and it never leaves the runtime.
        let write = crate::host_runtime::DurableWriteGuard::owner_uncounted(&self.lifecycle);
        let chrome = self.workspace_chrome_snapshot();
        if let Err(error) = session_store::save_chrome(&write, &chrome) {
            errors.push(format!("persist workspace chrome: {error:#}"));
        }
        let sessions: Vec<Uuid> = self.inner.lock().prompt_queues.keys().copied().collect();
        for session_id in sessions {
            if let Some(queue) = self.inner.lock().prompt_queues.get(&session_id).cloned() {
                if let Err(error) = session_store::save_prompt_queue(&write, session_id, &queue) {
                    errors.push(format!("persist prompt queue {session_id}: {error:#}"));
                }
            }
        }
        let subagents = self.inner.lock().subagents.clone();
        let subagent_sessions: HashSet<Uuid> = subagents
            .iter()
            .filter_map(|s| s.session_id.as_deref())
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect();
        for session_id in subagent_sessions {
            if let Err(error) =
                session_store::save_session_subagents(&write, session_id, &subagents)
            {
                errors.push(format!("persist subagents for {session_id}: {error:#}"));
            }
        }
        // Record the shutdown in the durable audit ledger while this process
        // still owns it, and surface a ledger failure instead of reporting a
        // clean stop over a lost record. The v2 ledger (#462 / #469) replaces
        // the sink behind these same calls without changing this seam.
        if let Some(store) = self.orchestration_store.lock().clone() {
            let entry = crate::orchestration::AuditEntry {
                ts: Utc::now(),
                tool: "host.shutdown".into(),
                request_id: None,
                session_id: None,
                workspace: None,
                outcome: "accepted".into(),
                error_code: None,
                detail: String::new(),
            };
            if let Err(error) = store.append_audit(&entry) {
                errors.push(format!("record host.shutdown audit entry: {error:#}"));
            }
            if let Some(audit_error) = store.last_audit_error() {
                errors.push(format!("durable audit ledger degraded: {audit_error}"));
            }
        }
        // Drop this process's ledger handles last: they must outlive every
        // supervised task that could still be writing.
        let orchestration = self.orchestration_store.lock().take();
        drop(orchestration);
        let computer = self.computer_store.lock().take();
        drop(computer);
        errors
    }

    fn provider_observation_context(&self, session_id: Uuid) -> Option<ProviderObservationContext> {
        let session = self.provider_observation.as_ref()?;
        let tracker = self.run_usage_trackers.lock().get(&session_id).cloned()?;
        session.context(tracker.run_id(), session_id).ok()
    }

    pub fn take_event_receiver(&self) -> Option<crate::event_bus::EventReceiver> {
        self.event_rx_factory.lock().take()
    }

    /// Shared fan-out event bus (GUI + MCP control plane).
    pub fn event_bus(&self) -> crate::event_bus::EventBus {
        self.inner.lock().event_tx.clone()
    }

    /// Additional live subscriber (does not steal the primary GUI receiver).
    pub fn subscribe_events(&self) -> crate::event_bus::EventReceiver {
        self.inner.lock().event_tx.subscribe()
    }

    /// Current model authority for the local Computer cockpit. Unknown models
    /// remain manual-only unless a durable provider profile or this process's
    /// explicit simulator qualification grants semantic authority.
    pub fn computer_agent_eligibility(&self, session_id: Uuid) -> Result<ComputerAgentEligibility> {
        if self
            .session_agent_authority(session_id)?
            .is_some_and(|policy| !policy.computer_use_allowed)
        {
            bail!("Computer Use is not allowed by this Agent specification");
        }
        let (model, _) = self.selected_computer_model(session_id)?;
        let credentials = crate::auth_store::resolve_wire_credentials_for_model(&model)
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow!(crate::auth_store::auth_help_message()))?;
        let resolved = resolve_computer_eligibility(&credentials, &model)?;
        if resolved.eligibility.tier >= crate::gateway_config::ComputerUseTier::SemanticAct {
            return Ok(resolved.eligibility);
        }
        let qualified = self
            .inner
            .lock()
            .computer_agent_qualifications
            .get(&(session_id, model.clone()))
            .is_some_and(|record| record.route_fingerprint == resolved.route_fingerprint);
        if qualified {
            return Ok(ComputerAgentEligibility {
                model,
                tier: crate::gateway_config::ComputerUseTier::SemanticAct,
                source: "session_measured".into(),
            });
        }
        Ok(resolved.eligibility)
    }

    /// Run the selected model against two deterministic simulator frames. No
    /// proposed action executes, and success is scoped to this exact route for
    /// the current process unless the provider profile already has a durable
    /// measured capability.
    pub async fn qualify_computer_agent(
        &self,
        session_id: Uuid,
    ) -> Result<ComputerAgentEligibility> {
        let (operation_id, cancel, _guard) = self.begin_computer_agent_operation(session_id)?;
        let (model, effort) = self.selected_computer_model(session_id)?;
        let credentials = crate::auth_store::resolve_wire_credentials_for_model(&model)
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow!(crate::auth_store::auth_help_message()))?;
        let resolved = resolve_computer_eligibility(&credentials, &model)?;
        if resolved.eligibility.tier >= crate::gateway_config::ComputerUseTier::SemanticAct {
            return Ok(resolved.eligibility);
        }
        qualify_semantic_model(&credentials, &model, effort, &cancel)
            .await
            .context("selected model did not pass bounded Computer qualification")?;
        if cancel.is_cancelled() {
            bail!("Computer model qualification was cancelled");
        }
        self.ensure_computer_route_unchanged(session_id, &model, &resolved.route_fingerprint)?;
        {
            let mut inner = self.inner.lock();
            if inner
                .computer_agent_operations
                .get(&session_id)
                .is_none_or(|(current, _)| current != &operation_id)
            {
                bail!("Computer model qualification was superseded");
            }
            inner.computer_agent_qualifications.insert(
                (session_id, model.clone()),
                SessionComputerQualification {
                    route_fingerprint: resolved.route_fingerprint,
                },
            );
        }
        Ok(ComputerAgentEligibility {
            model,
            tier: crate::gateway_config::ComputerUseTier::SemanticAct,
            source: "session_measured".into(),
        })
    }

    /// Ask the selected, qualified model for one bounded semantic proposal.
    /// This method cannot dispatch an OS action.
    pub async fn propose_computer_action(
        &self,
        session_id: Uuid,
        objective: &str,
        observation: &crate::computer_use::ComputerObservation,
    ) -> Result<ComputerAgentProposal> {
        let (_operation_id, cancel, _guard) = self.begin_computer_agent_operation(session_id)?;
        let (model, effort) = self.selected_computer_model(session_id)?;
        let credentials = crate::auth_store::resolve_wire_credentials_for_model(&model)
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow!(crate::auth_store::auth_help_message()))?;
        let resolved = resolve_computer_eligibility(&credentials, &model)?;
        let durable_authority =
            resolved.eligibility.tier >= crate::gateway_config::ComputerUseTier::SemanticAct;
        let session_authority = self
            .inner
            .lock()
            .computer_agent_qualifications
            .get(&(session_id, model.clone()))
            .is_some_and(|record| record.route_fingerprint == resolved.route_fingerprint);
        if !durable_authority && !session_authority {
            bail!("selected model is not qualified for semantic Computer actions");
        }
        let proposal = propose_semantic_action(
            &credentials,
            &model,
            effort,
            objective,
            observation,
            &cancel,
        )
        .await
        .context("selected model did not return a valid bounded Computer proposal")?;
        if cancel.is_cancelled() {
            bail!("Computer model proposal was cancelled");
        }
        self.ensure_computer_route_unchanged(session_id, &model, &resolved.route_fingerprint)?;
        Ok(proposal)
    }

    /// Local Stop/Take over cancellation. It does not share the Build-turn
    /// token, so cancelling Computer inference never cancels unrelated coding.
    pub fn cancel_computer_agent(&self, session_id: Uuid) -> bool {
        let token = self
            .inner
            .lock()
            .computer_agent_operations
            .remove(&session_id)
            .map(|(_, token)| token);
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    fn invalidate_computer_agent_authority(&self) {
        let tokens = {
            let mut inner = self.inner.lock();
            inner.computer_agent_qualifications.clear();
            inner
                .computer_agent_operations
                .drain()
                .map(|(_, (_, token))| token)
                .collect::<Vec<_>>()
        };
        for token in tokens {
            token.cancel();
        }
    }

    fn selected_computer_model(&self, session_id: Uuid) -> Result<(String, EffortLevel)> {
        let (ambient_model, effort) = {
            let inner = self.inner.lock();
            if !inner.sessions.contains_key(&session_id) {
                bail!("unknown session");
            }
            (inner.model.clone(), inner.effort)
        };
        let model = self
            .session_agent_spec(session_id)?
            .map(|spec| spec.model.selection_key)
            .unwrap_or(ambient_model);
        Ok((model, effort))
    }

    fn begin_computer_agent_operation(
        &self,
        session_id: Uuid,
    ) -> Result<(String, CancellationToken, ComputerAgentBusyGuard)> {
        self.ensure_session_accepts_new_work(session_id)?;
        if self
            .session_agent_authority(session_id)?
            .is_some_and(|policy| !policy.computer_use_allowed)
        {
            bail!("Computer Use is not allowed by this Agent specification");
        }
        let operation_id = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        {
            let mut inner = self.inner.lock();
            if !inner.sessions.contains_key(&session_id) {
                bail!("unknown session");
            }
            if inner.computer_agent_operations.contains_key(&session_id) {
                bail!("a Computer model request is already running for this session");
            }
            inner
                .computer_agent_operations
                .insert(session_id, (operation_id.clone(), cancel.clone()));
        }
        let guard = ComputerAgentBusyGuard {
            host: self.clone(),
            session_id,
            operation_id: operation_id.clone(),
        };
        Ok((operation_id, cancel, guard))
    }

    fn ensure_computer_route_unchanged(
        &self,
        session_id: Uuid,
        expected_model: &str,
        expected_route: &str,
    ) -> Result<()> {
        let (current_model, _) = self.selected_computer_model(session_id)?;
        if current_model != expected_model {
            bail!("selected model changed while the Computer request was running");
        }
        let credentials = crate::auth_store::resolve_wire_credentials_for_model(&current_model)
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow!(crate::auth_store::auth_help_message()))?;
        let current = resolve_computer_eligibility(&credentials, &current_model)?;
        if current.route_fingerprint != expected_route {
            bail!("provider route changed while the Computer request was running");
        }
        Ok(())
    }

    /// Shared wake-up for every embedded orchestration scheduler.
    pub(crate) fn orchestration_wakeup(&self) -> Arc<Notify> {
        self.orchestration_wakeup.clone()
    }

    /// Open the single process-owned durable run ledger on first use.
    pub fn ensure_orchestration_store(&self) -> Result<OrchStore> {
        // A stale handle must not reopen the durable ledger for a home this
        // process no longer owns (#455).
        self.ensure_accepting("opening the durable orchestration ledger")?;
        let mut store = self.orchestration_store.lock();
        if let Some(existing) = store.as_ref() {
            return Ok(existing.clone());
        }
        let opened = OrchStore::open(self.runtime_home.orchestration_root())?;
        // Bind explicitly rather than relying on open-time registry lookup, so
        // every clone of this ledger fails closed with this runtime. The bind
        // verifies the ledger's home is the one this runtime owns.
        // The ledger is under this runtime's home by construction, so this
        // always binds; a false here would mean the home moved underneath us.
        debug_assert!(opened.bind_lifecycle(&self.lifecycle));
        *store = Some(opened.clone());
        Ok(opened)
    }

    /// Open (or return) the single durable Computer Run ledger (#271). The
    /// store holds an exclusive file lock, so every surface — desktop cockpit
    /// and embedded MCP control plane alike — must share this handle; a
    /// second open in the same process would fail on the lock.
    pub fn ensure_computer_store(&self) -> Result<crate::computer_use::ComputerStore> {
        self.ensure_accepting("opening the durable Computer Run ledger")?;
        let mut store = self.computer_store.lock();
        if let Some(existing) = store.as_ref() {
            return Ok(existing.clone());
        }
        let opened = crate::computer_use::ComputerStore::open(self.runtime_home.computer_root())?;
        debug_assert!(opened.bind_lifecycle(&self.lifecycle));
        *store = Some(opened.clone());
        Ok(opened)
    }

    /// Return the already-open Computer Run ledger without filesystem work.
    pub fn computer_store(&self) -> Option<crate::computer_use::ComputerStore> {
        self.computer_store.lock().clone()
    }

    /// Adopt a store the caller opened, if this runtime has none yet. This
    /// keeps library/test construction and the desktop bootstrap on one
    /// durable ledger rather than silently splitting external and desktop run
    /// records.
    ///
    /// A ledger under this runtime's home is bound to its lifecycle, so every
    /// clone of it fails closed with this runtime. A ledger rooted elsewhere
    /// is still adopted as the process ledger but keeps the authority it
    /// established at open, because this runtime's instance lock does not
    /// protect that root — authority is never borrowed across homes (#455).
    pub(crate) fn install_orchestration_store(&self, store: OrchStore) -> bool {
        let mut current = self.orchestration_store.lock();
        if current.is_some() {
            return false;
        }
        if !store.bind_lifecycle(&self.lifecycle) {
            eprintln!(
                "[grokptah] adopting an orchestration ledger governed by {}, outside the home \
                 this runtime owns ({}); it keeps the durable-write authority it established at \
                 open rather than borrowing this runtime's",
                store.authority_home_lock().display(),
                self.lifecycle.lock_path().display()
            );
        }
        *current = Some(store);
        true
    }

    /// Return the already-open ledger without causing filesystem work.
    pub fn orchestration_store(&self) -> Option<OrchStore> {
        self.orchestration_store.lock().clone()
    }

    /// Return or create the durable agent identity for a Build session. The
    /// session owns the binding, while the orchestration store owns lifecycle
    /// state; this keeps transport adapters from inventing identity.
    pub fn ensure_session_agent(&self, session_id: Uuid) -> Result<AgentRecord> {
        let write = self.durable_write("ensuring a session agent")?;
        let (cwd, model, kind, existing_id, authority, default_bounds) = {
            let g = self.inner.lock();
            let selected_model = g.model.clone();
            let session = g
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let mut auto_allowed_tools = g.always_allowed_tools.iter().cloned().collect::<Vec<_>>();
            auto_allowed_tools.sort();
            let mut allowed_mcp_servers = g
                .mcp_servers
                .iter()
                .filter(|server| server.enabled)
                .map(|server| server.name.clone())
                .collect::<Vec<_>>();
            allowed_mcp_servers.sort();
            allowed_mcp_servers.dedup();
            let mut default_bounds = RunBounds {
                max_total_tokens: Some(
                    crate::orchestration::DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS,
                ),
                ..RunBounds::default()
            };
            if let Some(max_rounds) = g.max_agent_rounds {
                default_bounds.max_rounds = max_rounds.max(1);
            }
            (
                session.cwd.clone(),
                selected_model,
                session.kind,
                session.agent_id.clone(),
                AgentAuthorityPolicy {
                    sandbox_profile: normalize_sandbox_profile(&g.sandbox_profile).into(),
                    bypass_permissions: g.always_approve
                        || g.permission_mode == "bypassPermissions",
                    allowed_tools: DEFAULT_AGENT_TOOL_IDS
                        .iter()
                        .map(|tool| (*tool).to_string())
                        .collect(),
                    allowed_mcp_servers,
                    computer_use_allowed: false,
                    auto_allowed_tools,
                    allow_rules: g.allow_rules.clone(),
                    deny_rules: g.deny_rules.clone(),
                },
                default_bounds,
            )
        };
        if kind != SessionKind::Build {
            bail!("persistent agents are available only for Build sessions");
        }
        let store = self.ensure_orchestration_store()?;
        let workspace = cwd.display().to_string();
        let agent_id = existing_id
            .clone()
            .unwrap_or_else(|| format!("agent-{session_id}"));
        let now = Utc::now();
        let mut agent = match store.load_agent(&agent_id)? {
            Some(agent) => {
                if !agent.known_lane_ids().contains(&session_id) || agent.workspace != workspace {
                    bail!("session is bound to a different persistent agent workspace");
                }
                agent
            }
            None => {
                let mut spec =
                    AgentSpec::initial(&agent_id, &workspace, &model, authority, now, "desktop")
                        .map_err(|error| anyhow!(error.to_string()))?;
                spec.default_run_bounds = default_bounds;
                spec.validate()
                    .map_err(|error| anyhow!(error.to_string()))?;
                AgentRecord {
                    agent_id: agent_id.clone(),
                    owner_principal_id: None,
                    session_id,
                    lane_ids: vec![session_id],
                    lane_associations: vec![AgentLaneAssociation {
                        lane_id: session_id,
                        source_workspace: workspace.clone(),
                        attached_at: now,
                        attached_by: "desktop".into(),
                        detached_at: None,
                        detached_by: None,
                    }],
                    workspace: workspace.clone(),
                    model: model.clone(),
                    spec: Some(spec),
                    state: AgentState::Waiting,
                    current_run_id: None,
                    last_run_id: None,
                    last_lane_id: Some(session_id),
                    latest_checkpoint_id: None,
                    continuation_ordinal: 0,
                    created_at: now,
                    updated_at: now,
                }
            }
        };
        let was_associated = agent.known_lane_ids().contains(&session_id);
        let mut association_changed = false;
        if !agent.lane_ids.contains(&session_id) {
            agent.lane_ids.push(session_id);
            association_changed = true;
        }
        if !was_associated {
            agent.lane_associations.push(AgentLaneAssociation {
                lane_id: session_id,
                source_workspace: agent.workspace.clone(),
                attached_at: Utc::now(),
                attached_by: "desktop".into(),
                detached_at: None,
                detached_by: None,
            });
            association_changed = true;
        }
        if association_changed {
            agent.updated_at = now;
            store.save_agent(&agent)?;
        }
        if store.load_agent(&agent_id)?.is_none() {
            store.save_agent(&agent)?;
        }
        if existing_id.is_none() {
            let session = {
                let mut g = self.inner.lock();
                let session = g
                    .sessions
                    .get_mut(&session_id)
                    .ok_or_else(|| anyhow!("unknown session"))?;
                session.agent_id = Some(agent_id.clone());
                session.clone()
            };
            if let Err(error) = session_store::save_session_meta(&write, &session) {
                bail!("failed to persist session agent binding: {error:#}");
            }
        }
        Ok(agent)
    }

    /// Attach an existing Build session to a durable Agent identity.
    ///
    /// This is the first explicit many-Lanes bridge: the legacy
    /// `AgentRecord.session_id` remains the primary resume session, while
    /// `lane_ids` records every attached Lane. A Lane keeps its own workspace
    /// and transcript; attaching it never rewrites the Agent's legacy primary
    /// workspace or silently resumes a Run.
    pub fn attach_session_to_agent(&self, session_id: Uuid, agent_id: &str) -> Result<AgentRecord> {
        let write = self.durable_write("attaching a session to an agent")?;
        let (kind, session) = {
            let g = self.inner.lock();
            let session = g
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            (session.kind, session.clone())
        };
        if kind != SessionKind::Build {
            bail!("persistent agents are available only for Build sessions");
        }
        if let Some(existing) = session.agent_id.as_deref() {
            if existing != agent_id {
                bail!(
                    "Lane is already attached to a different Agent; detach it explicitly before reassignment"
                );
            }
        }
        let store = self.ensure_orchestration_store()?;
        let mut agent = store
            .load_agent(agent_id)?
            .ok_or_else(|| anyhow!("unknown persistent agent: {agent_id}"))?;
        if session.cwd.display().to_string() != agent.workspace {
            bail!("Lane workspace does not match the Agent source workspace");
        }
        let original_agent = agent.clone();
        let was_associated = agent.known_lane_ids().contains(&session_id);
        let mut association_changed = false;
        if !agent.lane_ids.contains(&session_id) {
            agent.lane_ids.push(session_id);
            association_changed = true;
        }
        if !was_associated {
            agent.lane_associations.push(AgentLaneAssociation {
                lane_id: session_id,
                source_workspace: agent.workspace.clone(),
                attached_at: Utc::now(),
                attached_by: "desktop".into(),
                detached_at: None,
                detached_by: None,
            });
            association_changed = true;
        }
        if association_changed {
            agent.updated_at = Utc::now();
            store.save_agent(&agent)?;
        }
        if session.agent_id.as_deref() != Some(agent_id) {
            let (persist_result, rollback_result) = {
                let mut g = self.inner.lock();
                let current = g
                    .sessions
                    .get_mut(&session_id)
                    .ok_or_else(|| anyhow!("unknown session"))?;
                current.agent_id = Some(agent_id.to_string());
                let updated = current.clone();
                let persist_result = session_store::save_session_meta(&write, &updated);
                let rollback_result = if persist_result.is_err() {
                    current.agent_id = session.agent_id.clone();
                    if association_changed {
                        store
                            .update_agent(agent_id, |durable| {
                                durable.lane_ids.retain(|lane| *lane != session_id);
                                if original_agent.lane_ids.contains(&session_id) {
                                    durable.lane_ids.push(session_id);
                                }
                                durable
                                    .lane_associations
                                    .retain(|association| association.lane_id != session_id);
                                durable.lane_associations.extend(
                                    original_agent
                                        .lane_associations
                                        .iter()
                                        .filter(|association| association.lane_id == session_id)
                                        .cloned(),
                                );
                                Ok(())
                            })
                            .map(|_| ())
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                };
                (persist_result, rollback_result)
            };
            if let Err(error) = persist_result {
                rollback_result.context("roll back Agent Lane association")?;
                return Err(error).context("persist Lane Agent binding");
            }
        }
        Ok(agent)
    }

    pub fn list_persistent_agents(&self) -> Result<Vec<AgentRecord>> {
        self.ensure_orchestration_store()?.list_agents()
    }

    /// List the product-facing Lane projection, including archived Lanes when
    /// requested. The backing session records remain the source of truth.
    pub fn list_lanes(&self, include_archived: bool) -> Vec<LaneSummary> {
        let g = self.inner.lock();
        let mut lanes: Vec<_> = g
            .sessions
            .values()
            .filter(|session| include_archived || !session.archived)
            .map(|session| LaneSummary::from(&session.summary()))
            .collect();
        lanes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        lanes
    }

    pub fn get_persistent_agent(&self, agent_id: &str) -> Result<Option<AgentRecord>> {
        self.ensure_orchestration_store()?.load_agent(agent_id)
    }

    /// Attributable enable/disable for native Work execution. Defaults remain off.
    pub fn set_managed_execution(
        &self,
        agent_id: &str,
        enabled: bool,
        actor: &str,
    ) -> Result<Option<AgentRecord>> {
        let store = self.ensure_orchestration_store()?;
        store
            .revise_agent_spec(agent_id, actor, |spec| {
                spec.managed_execution.enabled = enabled;
                if enabled {
                    spec.managed_execution.bounds.max_total_tokens = spec
                        .managed_execution
                        .bounds
                        .max_total_tokens
                        .or(spec.default_run_bounds.max_total_tokens)
                        .or(Some(
                            crate::orchestration::DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS,
                        ));
                    if spec.authority.computer_use_allowed {
                        anyhow::bail!("managed execution cannot grant Computer Use");
                    }
                    spec.authority.bypass_permissions = false;
                }
                spec.managed_execution
                    .validate()
                    .map_err(|error| anyhow::anyhow!(error.message))?;
                Ok(())
            })
            .map_err(|error| anyhow!(error.to_string()))
    }

    /// Read durable Work Items owned by one local Build Lane. Work Items are
    /// filtered by their persisted Lane id rather than focused UI state so
    /// archived and background work remains correctly attributable.
    pub fn list_work_items_for_session(&self, session_id: Uuid) -> Result<Vec<WorkItem>> {
        let store = self.ensure_orchestration_store()?;
        let mut items = store
            .list_work_items()
            .map_err(|error| anyhow!(error.to_string()))?
            .into_iter()
            .filter(|item| item.session_id == session_id)
            .collect::<Vec<_>>();
        items.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(items)
    }

    /// Read one local Work Item together with its redacted attempt history.
    pub fn get_work_item_snapshot(
        &self,
        session_id: Uuid,
        work_id: &str,
    ) -> Result<Option<WorkItemSnapshot>> {
        let store = self.ensure_orchestration_store()?;
        let Some(work) = store
            .load_work_item(work_id)
            .map_err(|error| anyhow!(error.to_string()))?
        else {
            return Ok(None);
        };
        if work.session_id != session_id {
            return Ok(None);
        }
        let attempts = store
            .list_work_attempts(Some(work_id))
            .map_err(|error| anyhow!(error.to_string()))?
            .iter()
            .map(WorkAttemptView::from)
            .collect();
        Ok(Some(WorkItemSnapshot { work, attempts }))
    }

    fn local_work_mutation_scope(&self, session_id: Uuid) -> Result<(String, OrchStore)> {
        self.ensure_session_accepts_new_work(session_id)?;
        let session = self.session_inspect(session_id)?;
        if session.kind != SessionKind::Build {
            bail!("only Build Lanes can own durable Work Items");
        }
        if session.cwd.trim().is_empty() {
            bail!("Build Lane has no workspace");
        }
        Ok((session.cwd, self.ensure_orchestration_store()?))
    }

    pub fn create_work_item(
        &self,
        session_id: Uuid,
        kind: String,
        objective: String,
        priority: i32,
        requires_approval: bool,
    ) -> Result<WorkItem> {
        let (workspace, store) = self.local_work_mutation_scope(session_id)?;
        let policy = WorkPolicy {
            requires_approval,
            ..WorkPolicy::default()
        };
        let mut item = WorkItem::new(kind, objective, session_id, workspace, "desktop", policy)
            .map_err(|error| anyhow!(error.to_string()))?;
        item.priority = priority;
        store
            .save_work_item(&item)
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(item)
    }

    pub fn assign_work_item(
        &self,
        session_id: Uuid,
        work_id: &str,
        assigned_agent_id: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<WorkItem> {
        let (_, store) = self.local_work_mutation_scope(session_id)?;
        let item = store
            .load_work_item(work_id)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("work item not found"))?;
        if item.session_id != session_id {
            bail!("work item is outside the selected Lane");
        }
        store
            .assign_work(work_id, assigned_agent_id, expected_revision)
            .map_err(|error| anyhow!(error.to_string()))
    }

    pub fn retry_work_item(
        &self,
        session_id: Uuid,
        work_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
    ) -> Result<WorkItem> {
        let (_, store) = self.local_work_mutation_scope(session_id)?;
        let item = store
            .load_work_item(work_id)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("work item not found"))?;
        if item.session_id != session_id {
            bail!("work item is outside the selected Lane");
        }
        store
            .retry_work(work_id, reason, expected_revision)
            .map_err(|error| anyhow!(error.to_string()))
    }

    pub fn approve_work_item(
        &self,
        session_id: Uuid,
        work_id: &str,
        note: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<WorkItem> {
        let (_, store) = self.local_work_mutation_scope(session_id)?;
        let item = store
            .load_work_item(work_id)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("work item not found"))?;
        if item.session_id != session_id {
            bail!("work item is outside the selected Lane");
        }
        store
            .approve_work(work_id, "desktop", note, expected_revision)
            .map(|(item, _)| item)
            .map_err(|error| anyhow!(error.to_string()))
    }

    pub fn cancel_work_item(
        &self,
        session_id: Uuid,
        work_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
    ) -> Result<WorkItem> {
        let (_, store) = self.local_work_mutation_scope(session_id)?;
        let item = store
            .load_work_item(work_id)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("work item not found"))?;
        if item.session_id != session_id {
            bail!("work item is outside the selected Lane");
        }
        store
            .cancel_work_checked(work_id, reason, expected_revision)
            .map(|(item, _)| item)
            .map_err(|error| anyhow!(error.to_string()))
    }

    pub fn list_routines_for_session(&self, session_id: Uuid) -> Result<Vec<RoutineRecord>> {
        let store = self.ensure_orchestration_store()?;
        let mut routines = store
            .list_routines()
            .map_err(|error| anyhow!(error.to_string()))?
            .into_iter()
            .filter(|routine| routine.session_id == session_id)
            .collect::<Vec<_>>();
        routines.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(routines)
    }

    pub fn get_routine_snapshot(
        &self,
        session_id: Uuid,
        routine_id: &str,
    ) -> Result<Option<RoutineSnapshot>> {
        let store = self.ensure_orchestration_store()?;
        let snapshot = store
            .routine_snapshot(routine_id, 32)
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(snapshot.filter(|snapshot| snapshot.routine.session_id == session_id))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_routine(
        &self,
        session_id: Uuid,
        name: String,
        agent_id: String,
        trigger: RoutineTrigger,
        work_template: WorkTemplate,
        missed_run_policy: MissedRunPolicy,
        concurrency: RoutineConcurrencyPolicy,
        retry: RoutineRetryPolicy,
    ) -> Result<RoutineRecord> {
        let (workspace, store) = self.local_work_mutation_scope(session_id)?;
        if matches!(trigger, RoutineTrigger::External { .. }) {
            bail!("webhook, GitHub, and message adapters are reserved for a later slice");
        }
        let agent = store
            .load_agent(&agent_id)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("unknown agent_id"))?;
        if !crate::orchestration::workspaces_match(&agent.workspace, &workspace) {
            bail!("agent source workspace does not match the Lane workspace");
        }
        let now = Utc::now();
        let routine = RoutineRecord::new(
            name,
            agent_id,
            session_id,
            workspace,
            trigger,
            work_template,
            missed_run_policy,
            concurrency,
            retry,
            "desktop",
            now,
        )
        .map_err(|error| anyhow!(error.to_string()))?;
        store
            .save_routine(&routine)
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(routine)
    }

    pub fn set_routine_lifecycle(
        &self,
        session_id: Uuid,
        routine_id: &str,
        lifecycle: RoutineLifecycle,
        expected_revision: Option<u64>,
    ) -> Result<RoutineRecord> {
        let (_, store) = self.local_work_mutation_scope(session_id)?;
        let routine = store
            .load_routine(routine_id)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("routine not found"))?;
        if routine.session_id != session_id {
            bail!("routine is outside the selected Lane");
        }
        store
            .set_routine_lifecycle(routine_id, lifecycle, expected_revision, Utc::now())
            .map_err(|error| anyhow!(error.to_string()))
    }

    pub fn fire_routine(
        &self,
        session_id: Uuid,
        routine_id: &str,
        request_id: &str,
    ) -> Result<crate::orchestration::ActivationRecord> {
        let (_, store) = self.local_work_mutation_scope(session_id)?;
        let routine = store
            .load_routine(routine_id)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("routine not found"))?;
        if routine.session_id != session_id {
            bail!("routine is outside the selected Lane");
        }
        let now = Utc::now();
        let request = crate::orchestration::ActivationRequest {
            cause: crate::orchestration::ActivationCause::Manual,
            dedupe_key: format!("manual:{routine_id}:{request_id}"),
            scheduled_at: now,
            received_at: now,
            payload: None,
            created_by: "desktop".into(),
        };
        store
            .activate_routine(routine_id, request, &RunBounds::default(), now)
            .map_err(|error| anyhow!(error.to_string()))
    }

    /// Validate a manual continuation against the durable agent, session, and
    /// latest checkpoint. No scheduling or automatic resume is implied.
    pub fn prepare_agent_resume(&self, session_id: Uuid) -> Result<AgentResumePlan> {
        let (workspace, agent_id) = {
            let g = self.inner.lock();
            let session = g
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            (
                session.cwd.display().to_string(),
                session
                    .agent_id
                    .clone()
                    .ok_or_else(|| anyhow!("session has no persistent agent identity"))?,
            )
        };
        let store = self.ensure_orchestration_store()?;
        let agent = store
            .load_agent(&agent_id)?
            .ok_or_else(|| anyhow!("persistent agent record is missing"))?;
        let checkpoint_id = agent
            .latest_checkpoint_id
            .as_deref()
            .ok_or_else(|| anyhow!("persistent agent has no verified checkpoint"))?;
        let checkpoint = store
            .load_checkpoint(checkpoint_id)?
            .ok_or_else(|| anyhow!("latest persistent checkpoint is missing"))?;
        if let Some(revision) = checkpoint.agent_spec_revision {
            if store.load_agent_spec(&agent.agent_id, revision)?.is_none() {
                bail!("checkpoint Agent specification revision is missing");
            }
        }
        let plan = AgentResumePlan {
            parent_run_id: checkpoint.run_id.clone(),
            agent,
            checkpoint,
        };
        plan.validate_for(session_id, &workspace)
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(plan)
    }

    /// Capture and persist the exact durable inputs and byte-bounded context
    /// for one explicit finite continuation Run.
    pub fn prepare_agent_continuation(
        &self,
        session_id: Uuid,
        instruction: &str,
        max_rounds: Option<u32>,
    ) -> Result<AgentContinuationPlan> {
        let plan = self.prepare_agent_resume(session_id)?;
        let store = self.ensure_orchestration_store()?;
        let execution_spec = plan
            .agent
            .current_spec()
            .map_err(|error| anyhow!(error.to_string()))?
            .clone();
        let mut effective_run_bounds = execution_spec.default_run_bounds.clone();
        if let Some(requested) = max_rounds {
            effective_run_bounds.max_rounds = effective_run_bounds.max_rounds.min(requested.max(1));
        }
        if let Some(ambient) = self.inner.lock().max_agent_rounds {
            effective_run_bounds.max_rounds = effective_run_bounds.max_rounds.min(ambient.max(1));
        }
        effective_run_bounds
            .validate()
            .map_err(|error| anyhow!(error.to_string()))?;

        let mut capture_reasons = Vec::new();
        let checkpoint_spec = match plan.checkpoint.agent_spec_revision {
            Some(revision) => store
                .load_agent_spec(&plan.agent.agent_id, revision)?
                .ok_or_else(|| anyhow!("checkpoint Agent specification revision is missing"))?,
            None => {
                capture_reasons.push(ContinuationReasonCode::LegacyCheckpointNoSpecRevision);
                execution_spec.clone()
            }
        };

        let lineage = self.capture_continuation_lineage(
            &store,
            &plan.agent,
            &plan.checkpoint,
            &mut capture_reasons,
        )?;
        let (memory_scopes, unavailable_memory_scopes) = self.capture_continuation_memory(
            &execution_spec,
            &plan.agent.agent_id,
            &mut capture_reasons,
        )?;
        let snapshot = crate::orchestration::ContinuationInputSnapshot::new(
            plan.agent.agent_id.clone(),
            session_id,
            execution_spec.source_workspace.clone(),
            plan.agent.runtime_state(),
            plan.checkpoint.clone(),
            checkpoint_spec,
            execution_spec,
            effective_run_bounds.clone(),
            instruction,
            lineage,
            memory_scopes,
            unavailable_memory_scopes,
            Vec::new(),
            capture_reasons,
        )
        .map_err(|error| anyhow!(error.to_string()))?;
        let context = assemble_continuation_context(&snapshot)
            .map_err(|failure| anyhow!("continuation assembly failed: {}", failure.detail))?;
        store.save_continuation_input(&snapshot)?;
        store.save_continuation_context(&context)?;
        Ok(AgentContinuationPlan {
            agent: plan.agent,
            checkpoint: plan.checkpoint,
            parent_run_id: plan.parent_run_id,
            effective_run_bounds,
            input_snapshot: snapshot,
            context,
        })
    }

    fn capture_continuation_lineage(
        &self,
        store: &OrchStore,
        agent: &AgentRecord,
        checkpoint: &ContinuationCheckpoint,
        capture_reasons: &mut Vec<ContinuationReasonCode>,
    ) -> Result<Vec<ContinuationRunInput>> {
        const MAX_LINEAGE_RUNS: usize = 8;
        let mut lineage = Vec::new();
        let mut next_run_id = Some(checkpoint.run_id.clone());
        let mut seen = HashSet::new();
        while let Some(run_id) = next_run_id.take() {
            if !seen.insert(run_id.clone()) {
                bail!("continuation lineage contains a cycle");
            }
            let Some(run) = store.load_run(&run_id)? else {
                if lineage.is_empty() {
                    bail!("checkpoint source Run is missing");
                }
                capture_reasons.push(ContinuationReasonCode::LineageAncestorMissing);
                capture_reasons.push(ContinuationReasonCode::LineageRetentionGap);
                break;
            };
            if run.agent_id.as_deref() != Some(agent.agent_id.as_str())
                || !Self::workspace_identity_matches(&run.workspace, &agent.workspace)
                || !run.state.is_terminal()
            {
                bail!("continuation lineage crosses Agent/workspace scope or is nonterminal");
            }
            if lineage.is_empty() && run.session_id != checkpoint.session_id {
                bail!("checkpoint source Run does not match its historical Lane");
            }
            next_run_id = run.parent_run_id.clone();
            let verification = run.aggregates.verification.as_ref();
            lineage.push(ContinuationRunInput {
                run_id: run.run_id,
                parent_run_id: run.parent_run_id,
                lane_id: run.session_id,
                state: run.state,
                stop_cause: run.stop_cause,
                terminal_result: run.terminal_result,
                final_response: run.final_response,
                progress_round: run.progress.as_ref().map(|progress| progress.round),
                progress_detail: run.progress.map(|progress| progress.detail),
                changed_files: run.aggregates.changes,
                tests: run
                    .aggregates
                    .tests
                    .into_iter()
                    .map(|test| ContinuationTestInput {
                        call_id: test.call_id,
                        command: test.command,
                        status: test.status,
                        exit_code: test.exit_code,
                        cancelled: test.cancelled,
                    })
                    .collect(),
                verification_status: verification.map(|value| value.status.clone()),
                verification_stop_reason: verification.map(|value| value.stop_reason.clone()),
            });
            if lineage.len() == MAX_LINEAGE_RUNS {
                if next_run_id.is_some() {
                    capture_reasons.push(ContinuationReasonCode::LineageLimitReached);
                }
                break;
            }
        }
        Ok(lineage)
    }

    fn capture_continuation_memory(
        &self,
        spec: &AgentSpec,
        agent_id: &str,
        capture_reasons: &mut Vec<ContinuationReasonCode>,
    ) -> Result<(Vec<ContinuationMemoryInput>, Vec<String>)> {
        let access = MemoryAccess::new(&spec.source_workspace, Some(agent_id.to_string()))
            .with_agent_policy(
                spec.memory.project_scope,
                spec.memory.agent_private_scope,
                spec.memory.team_ids.clone(),
            )?;
        let mut requested = Vec::new();
        if spec.memory.project_scope {
            requested.push((MemoryScope::Project, ContinuationMemoryScope::Project, None));
        }
        if spec.memory.agent_private_scope {
            requested.push((
                MemoryScope::AgentPrivate {
                    agent_id: agent_id.to_string(),
                },
                ContinuationMemoryScope::AgentPrivate,
                Some(agent_id.to_string()),
            ));
        }
        for team_id in &spec.memory.team_ids {
            requested.push((
                MemoryScope::Team {
                    team_id: team_id.clone(),
                },
                ContinuationMemoryScope::Team,
                Some(team_id.clone()),
            ));
        }

        let mut scopes = Vec::new();
        let mut unavailable = Vec::new();
        for (scope, rendered_scope, scope_id) in requested {
            let descriptor = match (&rendered_scope, scope_id.as_deref()) {
                (ContinuationMemoryScope::Project, _) => "project".into(),
                (ContinuationMemoryScope::AgentPrivate, Some(id)) => {
                    format!("agent_private:{id}")
                }
                (ContinuationMemoryScope::Team, Some(id)) => format!("team:{id}"),
                _ => "invalid".into(),
            };
            let facts = match access
                .resolve(scope)
                .and_then(|address| crate::memory::list_facts(&address))
            {
                Ok(facts) => facts,
                Err(_) => {
                    capture_reasons.push(ContinuationReasonCode::MemoryScopeUnavailable);
                    unavailable.push(descriptor);
                    continue;
                }
            };
            let mut normalized_facts = Vec::new();
            let mut invalid = false;
            for fact in facts {
                let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(&fact.updated_at) else {
                    invalid = true;
                    break;
                };
                normalized_facts.push(ContinuationMemoryFact {
                    id: fact.id,
                    text: fact.text,
                    tags: fact.tags,
                    updated_at: updated_at
                        .with_timezone(&Utc)
                        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                });
            }
            if invalid {
                capture_reasons.push(ContinuationReasonCode::MemoryScopeUnavailable);
                unavailable.push(descriptor);
                continue;
            }
            scopes.push(ContinuationMemoryInput {
                scope: rendered_scope,
                scope_id,
                facts: normalized_facts,
            });
        }
        Ok((scopes, unavailable))
    }

    /// Explicit manual resume seam for the current desktop bridge. The caller
    /// supplies the new user instruction; the verified checkpoint is injected
    /// as auditable system context and linked through `parent_run_id`.
    pub async fn resume_agent(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
    ) -> Result<String> {
        self.resume_agent_with_request_id(session_id, prompt, max_rounds, None)
            .await
    }

    /// Explicit resume with a caller-owned idempotency key. Reusing the same
    /// key and payload replays the completed response; a changed payload is a
    /// durable conflict and cannot start a second run.
    pub async fn resume_agent_with_request_id(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
        request_id: Option<String>,
    ) -> Result<String> {
        let store = self.ensure_orchestration_store()?;
        let request_id = request_id.unwrap_or_else(|| format!("resume-{}", Uuid::new_v4()));
        let bound_agent_id = self
            .inner
            .lock()
            .sessions
            .get(&session_id)
            .and_then(|session| session.agent_id.clone());
        let payload_hash = crate::orchestration::hash_payload(&serde_json::json!({
            "agentId": bound_agent_id,
            "targetLaneId": session_id,
            "instructionHash": crate::orchestration::hash_payload(&serde_json::json!(&prompt)),
            "instructionByteLength": prompt.len(),
            "maxRounds": max_rounds,
        }));
        match store.claim_idempotency("persistent_agent_resume", &request_id, &payload_hash)? {
            crate::orchestration::IdempotencyClaim::Replay(Ok(value)) => value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("idempotent resume response is not text")),
            crate::orchestration::IdempotencyClaim::Replay(Err(error)) => {
                Err(anyhow!(error.to_string()))
            }
            crate::orchestration::IdempotencyClaim::Pending => {
                bail!("resume request is already in progress")
            }
            crate::orchestration::IdempotencyClaim::Perform => {
                let plan = match self.prepare_agent_continuation(session_id, &prompt, max_rounds) {
                    Ok(plan) => plan,
                    Err(error) => {
                        let _ = store.fail_idempotency(
                            "persistent_agent_resume",
                            &request_id,
                            &payload_hash,
                            None,
                            crate::orchestration::OrchError::new(
                                crate::orchestration::OrchErrorCode::Conflict,
                                error.to_string(),
                            ),
                        );
                        return Err(error);
                    }
                };
                let context_id = plan.context.context_id.clone();
                let prior_run_ids: HashSet<_> = store
                    .list_runs()?
                    .into_iter()
                    .map(|run| run.run_id)
                    .collect();
                let result = self
                    .session_prompt_inner(session_id, prompt, max_rounds, None, None, Some(plan))
                    .await;
                let durable_run_id = store.list_runs()?.into_iter().find_map(|run| {
                    (!prior_run_ids.contains(&run.run_id)
                        && run.session_id == session_id
                        && run.continuation_context_id.as_deref() == Some(context_id.as_str()))
                    .then_some(run.run_id)
                });
                match result {
                    Ok(response) => {
                        store.complete_idempotency(
                            "persistent_agent_resume",
                            &request_id,
                            &payload_hash,
                            durable_run_id,
                            serde_json::Value::String(response.clone()),
                        )?;
                        Ok(response)
                    }
                    Err(error) => {
                        let _ = store.fail_idempotency(
                            "persistent_agent_resume",
                            &request_id,
                            &payload_hash,
                            durable_run_id,
                            crate::orchestration::OrchError::new(
                                crate::orchestration::OrchErrorCode::Internal,
                                error.to_string(),
                            ),
                        );
                        Err(error)
                    }
                }
            }
        }
    }

    /// Read desktop-visible runs for one session. Session scoping prevents a
    /// local inspector from displaying another workspace's coordinator data.
    pub fn list_session_runs(&self, session_id: Uuid) -> Result<Vec<RunRecord>> {
        let store = self.ensure_orchestration_store()?;
        Ok(store
            .list_runs()?
            .into_iter()
            .filter(|run| run.session_id == session_id)
            .collect())
    }

    /// Read one run only when it belongs to the requested session.
    pub fn get_session_run(&self, session_id: Uuid, run_id: &str) -> Result<Option<RunRecord>> {
        let store = self.ensure_orchestration_store()?;
        Ok(store
            .load_run(run_id)?
            .filter(|run| run.session_id == session_id))
    }

    /// Read the bounded journal range belonging to one durable run.
    pub fn get_session_run_events(
        &self,
        session_id: Uuid,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<JournalPage> {
        let run = self
            .get_session_run(session_id, run_id)?
            .ok_or_else(|| anyhow!("unknown run"))?;
        let Some(start_seq) = run.start_seq else {
            return Ok(JournalPage {
                entries: Vec::new(),
                next_cursor: None,
                cursor_expired: false,
            });
        };
        let mut page = self.event_bus().read_after(after_seq, limit);
        page.entries.retain(|entry| {
            session_id_of(&entry.update) == Some(session_id)
                && entry.seq >= start_seq
                && run.end_seq.map(|end| entry.seq <= end).unwrap_or(true)
        });
        Ok(page)
    }

    /// Read the bounded Git diff for an isolated terminal run.
    pub fn review_run(&self, session_id: Uuid, run_id: &str) -> Result<RunReview> {
        self.review_run_internal(session_id, run_id, true)
    }

    /// Inspect an isolated run without granting the desktop-only in-memory
    /// promotion marker. External coordinators must use durable approval.
    pub(crate) fn inspect_run(&self, session_id: Uuid, run_id: &str) -> Result<RunReview> {
        self.review_run_internal(session_id, run_id, false)
    }

    fn review_run_internal(
        &self,
        session_id: Uuid,
        run_id: &str,
        mark_reviewed: bool,
    ) -> Result<RunReview> {
        let store = self.ensure_orchestration_store()?;
        let run = store
            .load_run(run_id)?
            .filter(|run| run.session_id == session_id)
            .ok_or_else(|| anyhow!("unknown run"))?;
        if run.state != RunState::Completed {
            bail!("only completed isolated runs can be reviewed");
        }
        let execution = run
            .execution
            .as_ref()
            .ok_or_else(|| anyhow!("run used shared execution and has no isolated diff"))?;
        if execution.mode != RunExecutionMode::IsolatedWorktree {
            bail!("run used shared execution and has no isolated diff");
        }
        let source = canonical_session_workspace(self, session_id, &execution.source_workspace)?;
        run_promotion::validate_managed_worktree(
            &source,
            Path::new(&execution.execution_workspace),
        )?;
        let review = run_promotion::review(
            Path::new(&execution.execution_workspace),
            &execution.base_revision,
        )?;
        if execution.final_fingerprint.as_deref() != Some(review.fingerprint.as_str()) {
            let _ = store.update_run(run_id, |current| {
                if let Some(execution) = current.execution.as_mut() {
                    execution.promotion_state = PromotionState::Conflicted;
                }
                current.error_code = Some("promotion_conflict".into());
                current.updated_at = Utc::now();
                Ok(())
            });
            bail!("isolated worktree changed after the run; promotion is blocked");
        }
        if mark_reviewed {
            self.reviewed_runs.lock().insert(run_id.to_string());
        }
        Ok(review)
    }

    /// Promote an explicitly reviewed isolated run into its original clean
    /// source workspace. Repeated calls are idempotent when the final
    /// fingerprint is already present in the source workspace.
    pub fn promote_run(&self, session_id: Uuid, run_id: &str) -> Result<RunRecord> {
        self.promote_run_with_approval(session_id, run_id, None)
    }

    /// Promote a run using a persisted, exact-scope approval. Unlike the
    /// desktop-only review marker, this survives restart and is revalidated
    /// against the current worktree immediately before Git is changed.
    pub fn promote_run_with_approval(
        &self,
        session_id: Uuid,
        run_id: &str,
        approval_id: Option<&str>,
    ) -> Result<RunRecord> {
        self.with_promotion_lock(run_id, || {
            let store = self.ensure_orchestration_store()?;
            let mut run = store
                .load_run(run_id)?
                .filter(|run| run.session_id == session_id)
                .ok_or_else(|| anyhow!("unknown run"))?;
            if run.state != RunState::Completed {
                bail!("only completed runs can be promoted");
            }
            let execution = run
                .execution
                .clone()
                .ok_or_else(|| anyhow!("run used shared execution and cannot be promoted"))?;
            if execution.mode != RunExecutionMode::IsolatedWorktree {
                bail!("run used shared execution and cannot be promoted");
            }
            let durable_approval = run.approval.clone();
            if let Some(requested_id) = approval_id {
                let approval = durable_approval
                    .as_ref()
                    .ok_or_else(|| anyhow!("run has no persisted approval"))?;
                if approval.approval_id != requested_id {
                    bail!("approval does not belong to this run");
                }
            }
            if let Some(approval) = durable_approval.as_ref() {
                validate_run_approval(&run, approval, Utc::now())?;
            }
            if execution.promotion_state == PromotionState::Promoted {
                return Ok(run);
            }
            if execution.promotion_state != PromotionState::Ready {
                bail!("isolated run is not ready for promotion");
            }
            if let Some(approval) = durable_approval.as_ref() {
                validate_run_approval(&run, approval, Utc::now())?;
            }
            if !self.reviewed_runs.lock().contains(run_id) && durable_approval.is_none() {
                bail!("review the isolated run before promotion");
            }
            let source =
                canonical_session_workspace(self, session_id, &execution.source_workspace)?;
            let final_fingerprint = execution
                .final_fingerprint
                .as_deref()
                .ok_or_else(|| anyhow!("isolated run has no verified final fingerprint"))?;
            run_promotion::validate_managed_worktree(
                &source,
                Path::new(&execution.execution_workspace),
            )?;
            if let Some(approval) = durable_approval.as_ref() {
                let current_review = run_promotion::review(
                    Path::new(&execution.execution_workspace),
                    &execution.base_revision,
                )?;
                if current_review.fingerprint != approval.final_fingerprint
                    || current_review.changed_files != approval.changed_files
                {
                    bail!("isolated worktree changed after approval; review it again");
                }
            }
            let result = run_promotion::promote(
                &source,
                Path::new(&execution.execution_workspace),
                &execution.base_revision,
                &execution.source_fingerprint,
                final_fingerprint,
            );
            if let Err(error) = result {
                self.reviewed_runs.lock().remove(run_id);
                let _ = store.update_run(run_id, |current| {
                    if let Some(execution) = current.execution.as_mut() {
                        execution.promotion_state = PromotionState::Conflicted;
                    }
                    current.error_code = Some("promotion_conflict".into());
                    current.updated_at = Utc::now();
                    Ok(())
                });
                return Err(error);
            }
            run.execution
                .as_mut()
                .expect("execution was checked above")
                .promotion_state = PromotionState::Promoted;
            run.execution
                .as_mut()
                .expect("execution was checked above")
                .promoted_at = Some(Utc::now());
            run.error_code = None;
            run.updated_at = Utc::now();
            self.reviewed_runs.lock().remove(run_id);
            Ok(store
                .update_run(run_id, |current| {
                    *current = run.clone();
                    Ok(())
                })?
                .unwrap_or(run))
        })
    }

    /// Explicitly discard an isolated run's managed worktree.
    pub fn discard_run(&self, session_id: Uuid, run_id: &str) -> Result<RunRecord> {
        self.with_promotion_lock(run_id, || {
            let store = self.ensure_orchestration_store()?;
            let run = store
                .load_run(run_id)?
                .filter(|run| run.session_id == session_id)
                .ok_or_else(|| anyhow!("unknown run"))?;
            let mut execution = run
                .execution
                .clone()
                .ok_or_else(|| anyhow!("run used shared execution and has nothing to discard"))?;
            if execution.promotion_state == PromotionState::Promoted {
                bail!("a promoted run cannot be discarded from the source workspace");
            }
            let source =
                canonical_session_workspace(self, session_id, &execution.source_workspace)?;
            run_promotion::discard(&source, Path::new(&execution.execution_workspace))?;
            self.reviewed_runs.lock().remove(run_id);
            execution.promotion_state = PromotionState::Discarded;
            let updated = store.update_run(run_id, |current| {
                current.execution = Some(execution.clone());
                current.updated_at = Utc::now();
                Ok(())
            })?;
            updated.ok_or_else(|| anyhow!("run disappeared while discarding"))
        })
    }

    fn with_promotion_lock<T>(
        &self,
        run_id: &str,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if !self.promotion_locks.lock().insert(run_id.to_string()) {
            bail!("run promotion operation is already in progress");
        }
        let result = action();
        self.promotion_locks.lock().remove(run_id);
        result
    }

    #[allow(clippy::too_many_arguments)] // Keeps durable run identity inputs explicit.
    fn begin_desktop_run(
        &self,
        session_id: Uuid,
        cwd: &Path,
        prompt: &str,
        bounds: RunBounds,
        start_seq: u64,
        turn_id: Uuid,
        execution: Option<RunExecution>,
        agent_id: Option<String>,
        agent_spec_revision: Option<u64>,
        parent_run_id: Option<String>,
        continuation: Option<&AgentContinuationPlan>,
    ) -> Option<(String, OrchStore)> {
        let store = match self.ensure_orchestration_store() {
            Ok(store) => store,
            Err(error) => {
                eprintln!("[grokptah] desktop run ledger unavailable: {error:#}");
                return None;
            }
        };
        let run_id = format!("desktop-{turn_id}");
        let now = Utc::now();
        let durable_workspace = dunce::canonicalize(cwd)
            .unwrap_or_else(|_| cwd.to_path_buf())
            .display()
            .to_string();
        let run = RunRecord {
            run_id: run_id.clone(),
            session_id,
            workspace: durable_workspace,
            request_id: format!("desktop-turn-{turn_id}"),
            client_id: Some("desktop".into()),
            state: RunState::Running,
            purpose: RunPurpose::Execution,
            agent_id: agent_id.clone(),
            retry_of: None,
            parent_run_id,
            agent_spec_revision,
            checkpoint_id: continuation.map(|plan| plan.checkpoint.checkpoint_id.clone()),
            continuation_context_id: continuation.map(|plan| plan.context.context_id.clone()),
            continuation_context_hash: continuation.map(|plan| plan.context.prompt_sha256.clone()),
            continuation_fidelity: continuation
                .map(|plan| format!("{:?}", plan.context.fidelity).to_ascii_lowercase()),
            queue_position: None,
            bounds,
            prompt_preview: self
                .inner
                .lock()
                .event_tx
                .redact_text(&prompt_preview(prompt), 500),
            start_seq: Some(start_seq),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution,
            approval: None,
        };
        let persisted = match agent_id.as_deref() {
            Some(agent_id) => store.save_run_and_activate_agent(&run, agent_id),
            None => store.save_run(&run),
        };
        if let Err(error) = persisted {
            eprintln!("[grokptah] desktop run {run_id} start persistence failed: {error:#}");
            return None;
        }
        Some((run_id, store))
    }

    fn start_desktop_run_aggregator(
        &self,
        run_id: &str,
        session_id: Uuid,
        store: OrchStore,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let run_id = run_id.to_string();
        let mut receiver = self.subscribe_events();
        let shutdown = self.shutdown_token();
        self.spawn_supervised("starting a desktop run aggregator", async move {
            loop {
                tokio::select! {
                    update = receiver.recv() => {
                        let Some(update) = update else { break };
                        apply_run_aggregate(&store, &run_id, session_id, &update);
                    }
                    // Without this arm the aggregator would outlive shutdown
                    // and stall the join barrier forever.
                    _ = shutdown.cancelled() => break,
                }
            }
        })
        .ok()
    }

    fn checkpoint_context(
        &self,
        session_id: Uuid,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String> {
        let (summary, tail) = {
            let g = self.inner.lock();
            let session = g
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let tail = session
                .transcript
                .iter()
                .rev()
                .take(12)
                .cloned()
                .collect::<Vec<_>>();
            (session.compacted_summary.clone(), tail)
        };
        let mut raw = String::new();
        if let Some(summary) = summary {
            raw.push_str("Compacted summary:\n");
            raw.push_str(&summary);
            raw.push('\n');
        }
        raw.push_str("Recent durable transcript:\n");
        for entry in tail.into_iter().rev() {
            raw.push_str(&entry.role);
            raw.push_str(": ");
            raw.push_str(&entry.text);
            raw.push('\n');
        }
        let redacted = event_tx.redact_text(&raw, crate::orchestration::MAX_AGENT_CONTEXT_BYTES);
        let bounded = crate::textutil::truncate_at_char_boundary(
            &redacted,
            crate::orchestration::MAX_AGENT_CONTEXT_BYTES,
        )
        .trim()
        .to_string();
        if bounded.is_empty() {
            bail!("cannot create an empty persistent checkpoint context");
        }
        Ok(bounded)
    }

    pub(crate) fn persist_agent_checkpoint(
        &self,
        run: &RunRecord,
        outcome: &str,
        end_seq: u64,
        event_tx: &crate::event_bus::EventBus,
        store: &OrchStore,
    ) -> Result<()> {
        let result = self.persist_agent_checkpoint_inner(run, outcome, end_seq, event_tx, store);
        if result.is_err() {
            if let Some(agent_id) = run.agent_id.as_deref() {
                store.deactivate_agent_run(agent_id, &run.run_id, outcome == "failed")?;
            }
        }
        result
    }

    fn persist_agent_checkpoint_inner(
        &self,
        run: &RunRecord,
        outcome: &str,
        end_seq: u64,
        event_tx: &crate::event_bus::EventBus,
        store: &OrchStore,
    ) -> Result<()> {
        let Some(agent_id) = run.agent_id.as_deref() else {
            return Ok(());
        };
        let agent = store
            .load_agent(agent_id)?
            .ok_or_else(|| anyhow!("persistent agent record disappeared"))?;
        let context_summary = self.checkpoint_context(run.session_id, event_tx)?;
        let reason = match outcome {
            "completed" => ContinuationReason::TurnCompleted,
            "cancelled" => ContinuationReason::Cancelled,
            "limit_reached"
            | "max_duration_reached"
            | "max_rounds_reached"
            | "stationarity"
            | "recovery_exhausted"
            | "max_total_tokens_reached"
            | "max_total_tokens_usage_unavailable"
            | "max_total_tokens_accounting_overflow" => ContinuationReason::LimitReached,
            _ => ContinuationReason::Failed,
        };
        let mut checkpoint = ContinuationCheckpoint {
            checkpoint_id: format!("checkpoint-{}-{}", agent_id, Uuid::new_v4()),
            agent_id: agent_id.to_string(),
            session_id: run.session_id,
            run_id: run.run_id.clone(),
            agent_spec_revision: Some(
                run.agent_spec_revision
                    .unwrap_or(agent.current_spec()?.revision),
            ),
            parent_checkpoint_id: agent.latest_checkpoint_id.clone(),
            ordinal: agent.continuation_ordinal.saturating_add(1),
            workspace: run.workspace.clone(),
            context_summary,
            context_hash: String::new(),
            event_seq: end_seq,
            reason,
            created_at: Utc::now(),
        };
        checkpoint.context_hash = checkpoint.context_hash_for();
        store.save_checkpoint(&checkpoint)?;
        store
            .update_agent(agent_id, |current| {
                current.current_run_id = None;
                current.last_run_id = Some(run.run_id.clone());
                current.last_lane_id = Some(run.session_id);
                current.latest_checkpoint_id = Some(checkpoint.checkpoint_id.clone());
                current.continuation_ordinal = checkpoint.ordinal;
                current.state = if outcome == "failed" {
                    AgentState::Failed
                } else {
                    AgentState::Waiting
                };
                Ok(())
            })?
            .ok_or_else(|| anyhow!("persistent agent disappeared while checkpointing"))?;
        Ok(())
    }

    fn workspace_identity_matches(left: &str, right: &str) -> bool {
        if left == right {
            return true;
        }
        match (
            dunce::canonicalize(Path::new(left)),
            dunce::canonicalize(Path::new(right)),
        ) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }

    #[allow(clippy::too_many_arguments)] // Keeps terminal evidence inputs explicit at this boundary.
    async fn finalize_desktop_run(
        &self,
        run_id: &str,
        store: &OrchStore,
        session_id: Uuid,
        end_seq: u64,
        result: &Result<String>,
        outcome: &str,
        evidence: &crate::completion::CompletionEvidence,
        event_tx: &crate::event_bus::EventBus,
    ) {
        let Some(mut run) = store.load_run(run_id).ok().flatten() else {
            return;
        };
        if let Ok(entries) = event_tx.read_range_all(
            run.start_seq.map(|seq| seq.saturating_sub(1)).unwrap_or(0),
            Some(end_seq),
            Some(session_id),
        ) {
            for entry in entries {
                apply_run_aggregate(store, run_id, session_id, &entry.update);
            }
            run = store.load_run(run_id).ok().flatten().unwrap_or(run);
        }
        run.state = match outcome {
            "completed" => RunState::Completed,
            "cancelled" => RunState::Cancelled,
            "limit_reached"
            | "max_duration_reached"
            | "max_rounds_reached"
            | "stationarity"
            | "recovery_exhausted"
            | "max_total_tokens_reached"
            | "max_total_tokens_usage_unavailable"
            | "max_total_tokens_accounting_overflow" => RunState::LimitReached,
            _ => RunState::Failed,
        };
        run.end_seq = Some(end_seq);
        run.terminal_result = Some(outcome.into());
        let durable_token_error = run
            .error_code
            .as_deref()
            .is_some_and(|code| code.starts_with("max_total_tokens_"));
        if outcome == "completed" {
            run.error_code = None;
        } else if !durable_token_error {
            run.error_code = Some(outcome.into());
        }
        if run.stop_cause.is_none() {
            run.stop_cause = match outcome {
                "completed" => Some(RunStopCause::Completed),
                "cancelled" => Some(RunStopCause::Cancelled),
                "failed" => Some(RunStopCause::Failed),
                "max_duration_reached" => Some(RunStopCause::DurationLimit),
                _ => None,
            };
        }
        run.final_response = match result {
            Ok(text) => Some(event_tx.redact_text(text, 8_000)),
            Err(error) => Some(event_tx.redact_text(&error.to_string(), 2_000)),
        };
        run.aggregates.usage = evidence.usage.clone();
        run.aggregates.permissions_requested = evidence.observations.permissions_requested;
        run.aggregates.permissions_granted = evidence.observations.permissions_granted;
        run.aggregates.permissions_denied = evidence.observations.permissions_denied;
        run.aggregates.verification = Some(evidence.clone());
        if let Some(execution) = run.execution.as_mut() {
            if run.state == RunState::Completed {
                match run_promotion::snapshot(
                    Path::new(&execution.execution_workspace),
                    &execution.base_revision,
                ) {
                    Ok(snapshot) => {
                        execution.final_fingerprint = Some(snapshot.fingerprint);
                        execution.promotion_state = PromotionState::Ready;
                        if !snapshot.changed_files.is_empty() {
                            run.aggregates.changes = snapshot.changed_files;
                        }
                    }
                    Err(error) => {
                        eprintln!("[grokptah] isolated run {run_id} is not promotable: {error:#}");
                        execution.promotion_state = PromotionState::Conflicted;
                    }
                }
            } else {
                execution.promotion_state = PromotionState::Conflicted;
            }
        }
        run.updated_at = Utc::now();
        if let Err(error) = store.persist_finalization(&run) {
            eprintln!("[grokptah] desktop run {run_id} finalization failed: {error:#}");
        }
        if let Err(error) = self.persist_agent_checkpoint(&run, outcome, end_seq, event_tx, store) {
            eprintln!("[grokptah] persistent checkpoint for run {run_id} failed: {error:#}");
        }
    }

    /// Persist tiny workspace chrome (tabs / project / model) only.
    fn workspace_chrome_snapshot(&self) -> WorkspaceChrome {
        let g = self.inner.lock();
        WorkspaceChrome {
            version: 2,
            project_cwd: g.project_cwd.as_ref().map(|p| p.display().to_string()),
            active_session: g.active_session,
            open_tab_ids: g.open_tab_ids.clone(),
            model: g.model.clone(),
            effort: g.effort,
            sandbox_profile: g.sandbox_profile.clone(),
            appearance: g.appearance.clone(),
            always_approve: g.always_approve,
            subagent_isolation: g.subagent_isolation,
        }
    }

    pub fn persist_chrome(&self) {
        let write = match self.durable_write("persisting workspace chrome") {
            Ok(write) => write,
            Err(error) => {
                eprintln!("[grokptah] chrome persist refused: {error:#}");
                return;
            }
        };
        let chrome = self.workspace_chrome_snapshot();
        if let Err(e) = session_store::save_chrome(&write, &chrome) {
            eprintln!("[grokptah] chrome persist failed: {e:#}");
        }
    }

    /// Append new transcript lines + refresh meta for one session.
    pub fn persist_session(&self, id: Uuid) {
        let write = match self.durable_write("persisting a session") {
            Ok(write) => write,
            Err(error) => {
                eprintln!("[grokptah] persisting a session refused: {error:#}");
                return;
            }
        };
        let mut session = {
            let g = self.inner.lock();
            match g.sessions.get(&id) {
                Some(s) => s.clone(),
                None => return,
            }
        };
        // Ensure we only append what isn't on disk yet.
        if !session.transcript_loaded {
            // Still push meta (title/count) without loading body.
            if let Err(e) = session_store::save_session_meta(&write, &session) {
                eprintln!("[grokptah] meta persist failed: {e:#}");
            }
            return;
        }
        let from = session.persisted_len;
        match session_store::append_transcript(&write, &session, from) {
            Ok(n) => {
                session.persisted_len += n;
                let mut g = self.inner.lock();
                if let Some(s) = g.sessions.get_mut(&id) {
                    s.persisted_len = session.persisted_len;
                }
            }
            Err(e) => eprintln!("[grokptah] transcript append failed: {e:#}"),
        }
        // Always refresh meta (compact cursor, title, counts) even when no new lines.
        if let Err(e) = session_store::save_session_meta(&write, &session) {
            eprintln!("[grokptah] meta persist failed: {e:#}");
        }
    }

    /// Full transcript rewrite (rewind / fork only — never used by compact).
    pub fn persist_session_rewrite(&self, id: Uuid) {
        let write = match self.durable_write("rewriting a session transcript") {
            Ok(write) => write,
            Err(error) => {
                eprintln!("[grokptah] rewriting a session transcript refused: {error:#}");
                return;
            }
        };
        let session = {
            let g = self.inner.lock();
            match g.sessions.get(&id) {
                Some(s) => s.clone(),
                None => return,
            }
        };
        if let Err(e) = session_store::rewrite_transcript(&write, &session) {
            eprintln!("[grokptah] transcript rewrite failed: {e:#}");
            return;
        }
        let mut g = self.inner.lock();
        if let Some(s) = g.sessions.get_mut(&id) {
            s.persisted_len = s.transcript.len();
            s.transcript_loaded = true;
        }
    }

    /// Back-compat alias used by older call sites — chrome only.
    pub fn persist(&self) {
        self.persist_chrome();
    }

    /// UI restore: sessions list + which tabs were open.
    pub fn workspace_ui_state(&self) -> WorkspaceUiState {
        let g = self.inner.lock();
        // Active (non-archived) only for default restore list.
        let mut sessions: Vec<_> = g
            .sessions
            .values()
            .filter(|s| !s.archived)
            .map(|s| s.summary())
            .collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        // Lanes are the archive-aware product projection; the legacy
        // `sessions` list remains active-only for restore compatibility.
        let mut all_sessions: Vec<_> = g.sessions.values().map(|s| s.summary()).collect();
        all_sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let lanes = all_sessions.iter().map(LaneSummary::from).collect();
        WorkspaceUiState {
            project_cwd: g.project_cwd.as_ref().map(|p| p.display().to_string()),
            active_session: g.active_session,
            active_lane_id: g.active_session,
            open_tab_ids: g.open_tab_ids.clone(),
            model: g.model.clone(),
            effort: g.effort,
            sessions,
            lanes,
        }
    }

    /// Remember open tabs (call when the tab strip changes).
    pub fn set_open_tabs(&self, ids: Vec<Uuid>, _active: Option<Uuid>) {
        {
            let mut g = self.inner.lock();
            g.open_tab_ids = ids
                .into_iter()
                .filter(|id| g.sessions.contains_key(id))
                .collect();
        }
        self.persist_chrome();
    }

    /// Ensure transcript is in memory (lazy load from JSONL).
    pub fn ensure_transcript_loaded(&self, id: Uuid) -> Result<()> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        if s.transcript_loaded {
            return Ok(());
        }
        session_store::load_transcript(s)?;
        Ok(())
    }

    pub fn status(&self) -> AgentStatus {
        let g = self.inner.lock();
        AgentStatus {
            running: g.running,
            project_cwd: g.project_cwd.as_ref().map(|p| p.display().to_string()),
            active_session: g.active_session,
            always_approve: g.always_approve,
            model: g.model.clone(),
            effort: g.effort,
            sandbox_profile: g.sandbox_profile.clone(),
            appearance: g.appearance.clone(),
            auto_update_enabled: crate::desktop_auto_update_enabled(),
        }
    }

    pub fn start(&self) -> Result<()> {
        // Construction cannot succeed without the instance lock, so the only
        // way to fail here is a handle that outlived its runtime (#455).
        self.lifecycle.ensure_open("starting the agent host")?;
        self.inner.lock().running = true;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.invalidate_computer_agent_authority();
        let mut g = self.inner.lock();
        g.turn_generations.clear();
        for (_, c) in g.turn_cancels.drain() {
            c.cancel();
        }
        g.running = false;
        Ok(())
    }

    pub fn set_project_cwd(&self, path: impl AsRef<Path>) -> Result<String> {
        let _write = self.durable_write("selecting the project directory")?;
        let p = path.as_ref().to_path_buf();
        if !p.is_dir() {
            bail!("not a directory: {}", p.display());
        }
        let mcp = crate::discover::load_mcp_servers(Some(&p));
        let skills = crate::discover::discover_skills(Some(&p));
        {
            let mut g = self.inner.lock();
            g.project_cwd = Some(p.clone());
            g.mcp_servers = mcp;
            g.skills = skills;
        }
        self.persist_chrome();
        Ok(p.display().to_string())
    }

    pub fn session_new(&self) -> Result<SessionSummary> {
        self.session_new_kind(SessionKind::Build)
    }

    pub fn session_new_kind(&self, kind: SessionKind) -> Result<SessionSummary> {
        let _write = self.durable_write("creating a session")?;
        let summary = {
            let mut g = self.inner.lock();
            if !g.running {
                bail!("agent not started");
            }
            let cwd = g
                .project_cwd
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let model = g.model.clone();
            let effort = g.effort;
            let s = Session::new_with_kind(cwd, model, effort, kind);
            let summary = s.summary();
            g.active_session = Some(s.id);
            if !g.open_tab_ids.contains(&s.id) {
                g.open_tab_ids.push(s.id);
            }
            g.sessions.insert(s.id, s);
            summary
        };
        // Empty shell: meta + empty transcript file.
        self.persist_session_rewrite(summary.id);
        self.persist_chrome();
        Ok(summary)
    }

    /// Hybrid / keyword / semantic search over chats + builds.
    pub fn search_sessions(&self, query: SearchQuery) -> Result<Vec<SearchHit>> {
        search_engine::search(&query).map_err(|e| anyhow!(e))
    }

    pub fn session_load(&self, id: Uuid) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        // Build sessions pin their own project root — promote it so files/git
        // panels track the session you just opened.
        let (kind, cwd) = {
            let g = self.inner.lock();
            let s = g
                .sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            if s.archived {
                bail!("archived Lane is inspection-only; restore it before opening for work");
            }
            (s.kind, s.cwd.clone())
        };
        // A missing session workspace is recoverable, not permission to run in
        // whichever project happens to be open. Rebinding is explicit through
        // session_set_cwd, normally driven by the desktop folder picker.
        let workspace_state = workspace_status(&cwd);
        if kind == SessionKind::Build && workspace_state != WorkspaceStatus::Ready {
            bail!(
                "Lane workspace is {}; choose a valid workspace before resuming work",
                workspace_state.as_str().replace('_', " ")
            );
        }
        let workspace_projection = (kind == SessionKind::Build).then(|| {
            (
                crate::discover::load_mcp_servers(Some(&cwd)),
                crate::discover::discover_skills(Some(&cwd)),
            )
        });
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let summary = s.summary();
            if let Some((mcp, skills)) = workspace_projection {
                g.project_cwd = Some(cwd);
                g.mcp_servers = mcp;
                g.skills = skills;
            }
            g.active_session = Some(id);
            if !g.open_tab_ids.contains(&id) {
                g.open_tab_ids.push(id);
            }
            summary
        };
        // #152: restore historical subagent summary when reopening a session.
        self.load_session_subagents(id);
        self.persist_chrome();
        Ok(summary)
    }

    /// Read an archived or active Lane without promoting its workspace,
    /// changing the active Lane, or adding it to the persisted tab strip.
    pub fn session_inspect(&self, id: Uuid) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        let g = self.inner.lock();
        g.sessions
            .get(&id)
            .map(Session::summary)
            .ok_or_else(|| anyhow!("unknown session"))
    }

    /// Reject work/state mutations for an archived Lane while leaving read and
    /// explicit recovery operations available.
    pub fn ensure_session_accepts_new_work(&self, id: Uuid) -> Result<()> {
        // Ordered shutdown rejects new admissions before anything is torn
        // down, and stale handles stay rejected forever (#455). This is the
        // single seam shared by desktop turns, orchestration reservations,
        // queued admissions and Computer Use, so one check closes them all.
        self.ensure_accepting("admitting new work")?;
        let g = self.inner.lock();
        let session = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        if session.archived {
            bail!("archived Lane is inspection-only; restore it before starting new work");
        }
        Ok(())
    }

    /// Full transcript for hydrating a session tab (loads JSONL on demand).
    pub fn session_transcript(&self, id: Uuid) -> Result<Vec<TranscriptEntry>> {
        self.ensure_transcript_loaded(id)?;
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        Ok(s.transcript.clone())
    }

    /// Return durable completion evidence in chronological order.
    pub fn session_completion_history(&self, id: Uuid) -> Result<Vec<SessionCompletion>> {
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        Ok(s.completion_history.clone())
    }

    /// Persist one bounded, turn-correlated completion summary without adding
    /// redacted evidence blobs to the append-only transcript.
    pub fn record_completion_evidence(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        evidence: crate::completion::CompletionEvidence,
    ) -> Result<()> {
        let write = self.durable_write("recording completion evidence")?;
        let snapshot = {
            let mut g = self.inner.lock();
            let session = g
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            session
                .completion_history
                .retain(|item| item.turn_id != turn_id);
            session.completion_history.push(SessionCompletion {
                turn_id,
                completed_at: Utc::now(),
                evidence,
            });
            if session.completion_history.len() > MAX_SESSION_COMPLETION_HISTORY {
                let remove = session
                    .completion_history
                    .len()
                    .saturating_sub(MAX_SESSION_COMPLETION_HISTORY);
                session.completion_history.drain(0..remove);
            }
            session.updated_at = Utc::now();
            session.clone()
        };
        session_store::save_session_meta(&write, &snapshot)
    }

    /// Whether a session currently has an in-flight turn.
    pub fn session_busy(&self, id: Uuid) -> bool {
        let g = self.inner.lock();
        g.turn_cancels.contains_key(&id) || g.turn_reservations.contains_key(&id)
    }

    pub fn reserve_orchestration_turn(&self, run_id: &str, session_id: Uuid) -> Result<()> {
        self.ensure_session_accepts_new_work(session_id)?;
        let mut g = self.inner.lock();
        if !g.sessions.contains_key(&session_id) {
            bail!("unknown session");
        }
        if g.turn_cancels.contains_key(&session_id) || g.turn_reservations.contains_key(&session_id)
        {
            bail!("session already has an active turn");
        }
        if g.orchestration_admissions.len() >= g.orchestration_admission_limit {
            bail!("max concurrent runs reached");
        }
        g.turn_reservations.insert(session_id, run_id.to_string());
        g.orchestration_admissions
            .insert(run_id.to_string(), session_id);
        Ok(())
    }

    pub fn release_orchestration_turn(&self, run_id: &str) {
        let released = {
            let mut g = self.inner.lock();
            if let Some(session_id) = g.orchestration_admissions.remove(run_id) {
                if g.turn_reservations.get(&session_id).map(String::as_str) == Some(run_id) {
                    g.turn_reservations.remove(&session_id);
                }
                true
            } else {
                false
            }
        };
        if released {
            self.orchestration_wakeup.notify_waiters();
        }
    }

    pub fn orchestration_active_count(&self) -> usize {
        self.inner.lock().orchestration_admissions.len()
    }

    /// Register one process-wide pending admission. Keeping this ledger on
    /// the host prevents multiple embedded control services from multiplying
    /// their local queue limits into an unbounded prompt store, while the
    /// sequence number gives the scheduler one ordering domain.
    pub fn reserve_orchestration_queue_slot(&self, run_id: &str, session_id: Uuid) -> Result<()> {
        self.ensure_session_accepts_new_work(session_id)?;
        const MAX_PENDING_ADMISSIONS: usize = 32;
        let mut g = self.inner.lock();
        if let Some(existing) = g.orchestration_pending_admissions.get(run_id) {
            if existing.session_id != session_id {
                bail!("pending admission is owned by another session");
            }
            return Ok(());
        }
        if g.orchestration_pending_admissions.len() >= MAX_PENDING_ADMISSIONS {
            bail!("bounded admission queue is full ({MAX_PENDING_ADMISSIONS} pending runs)");
        }
        let sequence = g.orchestration_next_pending_sequence;
        g.orchestration_next_pending_sequence = sequence.saturating_add(1);
        g.orchestration_pending_admissions.insert(
            run_id.to_string(),
            OrchestrationPendingAdmission {
                session_id,
                sequence,
            },
        );
        Ok(())
    }

    pub fn release_orchestration_queue_slot(&self, run_id: &str) -> bool {
        self.inner
            .lock()
            .orchestration_pending_admissions
            .remove(run_id)
            .is_some()
    }

    pub fn orchestration_pending_count(&self) -> usize {
        self.inner.lock().orchestration_pending_admissions.len()
    }

    /// Return the current one-based global arrival position for a queued run.
    /// This is computed from the host ledger rather than a service-local
    /// queue, so it stays truthful when another embedded service enqueues or
    /// cancels work.
    pub fn orchestration_pending_position(&self, run_id: &str) -> Option<usize> {
        let g = self.inner.lock();
        let target = g.orchestration_pending_admissions.get(run_id)?.sequence;
        Some(
            g.orchestration_pending_admissions
                .values()
                .filter(|pending| pending.sequence <= target)
                .count(),
        )
    }

    /// Atomically select a globally fair pending run and reserve its active
    /// turn. Returning false means this service should leave its local queue
    /// untouched; another embedded service may own the globally eligible run.
    pub fn claim_orchestration_pending(&self, run_id: &str, session_id: Uuid) -> bool {
        let mut g = self.inner.lock();
        if g.sessions
            .get(&session_id)
            .is_none_or(|session| session.archived)
        {
            return false;
        }
        let Some(requested) = g.orchestration_pending_admissions.get(run_id).copied() else {
            return false;
        };
        if requested.session_id != session_id
            || g.orchestration_admissions.len() >= g.orchestration_admission_limit
            || g.turn_cancels.contains_key(&session_id)
            || g.turn_reservations.contains_key(&session_id)
        {
            return false;
        }

        // Only the oldest pending run for each session is eligible. This
        // preserves per-session FIFO while allowing different sessions to
        // share one global fairness decision across service instances.
        let mut oldest_by_session: HashMap<Uuid, (String, u64)> = HashMap::new();
        for (pending_id, pending) in &g.orchestration_pending_admissions {
            let entry = oldest_by_session
                .entry(pending.session_id)
                .or_insert_with(|| (pending_id.clone(), pending.sequence));
            if pending.sequence < entry.1 {
                *entry = (pending_id.clone(), pending.sequence);
            }
        }
        let mut eligible = oldest_by_session
            .values()
            .filter(|(_, sequence)| {
                g.orchestration_pending_admissions
                    .values()
                    .find(|pending| pending.sequence == *sequence)
                    .is_some_and(|pending| {
                        !g.turn_cancels.contains_key(&pending.session_id)
                            && !g.turn_reservations.contains_key(&pending.session_id)
                    })
            })
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            return false;
        }
        eligible.sort_by_key(|(_, sequence)| *sequence);
        let selected = g
            .orchestration_last_started_session
            .and_then(|last| {
                eligible
                    .iter()
                    .find(|(pending_id, _)| {
                        g.orchestration_pending_admissions
                            .get(pending_id)
                            .is_some_and(|pending| pending.session_id != last)
                    })
                    .copied()
            })
            .or_else(|| eligible.first().copied());
        let Some((selected_id, _)) = selected else {
            return false;
        };
        if selected_id != run_id {
            return false;
        }

        g.orchestration_pending_admissions.remove(run_id);
        g.turn_reservations.insert(session_id, run_id.to_string());
        g.orchestration_admissions
            .insert(run_id.to_string(), session_id);
        g.orchestration_last_started_session = Some(session_id);
        true
    }

    pub fn configure_orchestration_capacity(&self, limit: usize) -> usize {
        let mut g = self.inner.lock();
        g.orchestration_admission_limit = g.orchestration_admission_limit.min(limit.max(1));
        g.orchestration_admission_limit
    }

    pub fn orchestration_capacity_limit(&self) -> usize {
        self.inner.lock().orchestration_admission_limit
    }

    pub async fn wait_turn_idle(&self, session_id: Uuid) {
        while self.session_busy(session_id) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Release an unconsumed reservation owned by `owner`.
    pub fn release_turn_reservation(&self, session_id: Uuid, owner: &str) -> bool {
        let mut g = self.inner.lock();
        if g.turn_reservations.get(&session_id).map(String::as_str) == Some(owner) {
            g.turn_reservations.remove(&session_id);
            g.drain_reservations.remove(&session_id);
            drop(g);
            self.orchestration_wakeup.notify_waiters();
            true
        } else {
            false
        }
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        self.list_sessions_filtered(false)
    }

    /// When `archived_only` is true, return only archived; otherwise only active.
    pub fn list_sessions_filtered(&self, archived_only: bool) -> Vec<SessionSummary> {
        self.list_sessions_ex(archived_only, None)
    }

    /// Optional kind filter: Some(Chat) / Some(Build) / None = all kinds.
    pub fn list_sessions_ex(
        &self,
        archived_only: bool,
        kind: Option<SessionKind>,
    ) -> Vec<SessionSummary> {
        let g = self.inner.lock();
        let mut v: Vec<_> = g
            .sessions
            .values()
            .filter(|s| s.archived == archived_only)
            .filter(|s| kind.map(|k| s.kind == k).unwrap_or(true))
            .map(|s| s.summary())
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn list_all_sessions(&self) -> Vec<SessionSummary> {
        let g = self.inner.lock();
        let mut v: Vec<_> = g.sessions.values().map(|s| s.summary()).collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn list_sessions_by_kind(
        &self,
        kind: SessionKind,
        include_archived: bool,
    ) -> Vec<SessionSummary> {
        let g = self.inner.lock();
        let mut v: Vec<_> = g
            .sessions
            .values()
            .filter(|s| s.kind == kind)
            .filter(|s| include_archived || !s.archived)
            .map(|s| s.summary())
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn session_rename(&self, id: Uuid, title: String) -> Result<SessionSummary> {
        let _write = self.durable_write("renaming a session")?;
        let title = title.trim().to_string();
        if title.is_empty() {
            bail!("title must not be empty");
        }
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.title = title;
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id)?;
        Ok(summary)
    }

    pub fn session_delete(&self, id: Uuid) -> Result<()> {
        let write = self.durable_write("deleting a session")?;
        self.cancel_computer_agent(id);
        {
            let mut g = self.inner.lock();
            if !g.sessions.contains_key(&id) {
                bail!("unknown session");
            }
            if g.turn_cancels.contains_key(&id) {
                bail!("cannot delete a session with an active turn — stop it first");
            }
            g.sessions.remove(&id);
            g.computer_agent_qualifications
                .retain(|(session_id, _), _| *session_id != id);
            g.prompt_queues.remove(&id);
            g.open_tab_ids.retain(|t| *t != id);
            if g.active_session == Some(id) {
                // Only session_load may promote a replacement Lane and its
                // workspace atomically. The desktop will focus a surviving tab.
                g.active_session = None;
            }
        }
        session_store::delete_session(&write, id)?;
        self.persist_chrome();
        Ok(())
    }

    pub fn session_archive(&self, id: Uuid, archived: bool) -> Result<SessionSummary> {
        let _write = self.durable_write("archiving a session")?;
        let summary = {
            let mut g = self.inner.lock();
            if archived && g.turn_cancels.contains_key(&id) {
                bail!("cannot archive a Lane with an active turn — stop it first");
            }
            {
                let s = g
                    .sessions
                    .get_mut(&id)
                    .ok_or_else(|| anyhow!("unknown session"))?;
                s.archived = archived;
                s.archived_at = if archived { Some(Utc::now()) } else { None };
                s.updated_at = Utc::now();
            }
            // Archived tabs remain durable for read-only inspection, but an
            // archived Lane can never own the live workspace/tool scope.
            if archived && g.active_session == Some(id) {
                g.active_session = None;
            }
            g.sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?
                .summary()
        };
        self.persist_session_meta_only(id)?;
        self.persist_chrome();
        Ok(summary)
    }

    pub fn session_set_folder(&self, id: Uuid, folder: Option<String>) -> Result<SessionSummary> {
        let folder = folder.and_then(|f| {
            let t = f.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        });
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.folder = folder;
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id)?;
        Ok(summary)
    }

    /// Set the working directory for a session (tools + shell run here).
    ///
    /// For build sessions this is the project root. When the session is active,
    /// also updates the host project cwd so the files/git panels match.
    pub fn session_set_cwd(&self, id: Uuid, path: impl AsRef<Path>) -> Result<SessionSummary> {
        let _write = self.durable_write("selecting a session directory")?;
        self.ensure_session_accepts_new_work(id)?;
        let p = path.as_ref().to_path_buf();
        if !p.is_dir() {
            bail!("not a directory: {}", p.display());
        }
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.cwd = p.clone();
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id)?;

        // Keep host workspace + discovery in sync when this is the focused session
        // or when no project is open yet.
        let should_sync = {
            let g = self.inner.lock();
            g.active_session == Some(id) || g.project_cwd.is_none()
        };
        if should_sync {
            let _ = self.set_project_cwd(&p);
        }
        Ok(summary)
    }

    /// Set the execution policy for future Build turns in one session.
    /// Shared execution remains the default; changing policy during a turn is
    /// refused so a running model can never change workspaces underneath it.
    pub fn session_set_execution_mode(
        &self,
        id: Uuid,
        mode: RunExecutionMode,
    ) -> Result<SessionSummary> {
        self.ensure_session_accepts_new_work(id)?;
        let summary = {
            let mut g = self.inner.lock();
            if g.turn_cancels.contains_key(&id) {
                bail!("cannot change execution mode while a turn is running");
            }
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            if s.kind != SessionKind::Build && mode != RunExecutionMode::Shared {
                bail!("isolated execution is available only for Build sessions");
            }
            s.execution_mode = mode;
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id)?;
        Ok(summary)
    }

    pub fn session_set_tags(&self, id: Uuid, tags: Vec<String>) -> Result<SessionSummary> {
        let mut clean: Vec<String> = tags
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        clean.sort();
        clean.dedup();
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.tags = clean;
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id)?;
        Ok(summary)
    }

    /// Unique folder names from non-archived sessions (plus any archived if requested).
    pub fn list_folders(&self, include_archived: bool) -> Vec<String> {
        let g = self.inner.lock();
        let mut set = std::collections::BTreeSet::new();
        for s in g.sessions.values() {
            if !include_archived && s.archived {
                continue;
            }
            if let Some(f) = &s.folder {
                if !f.is_empty() {
                    set.insert(f.clone());
                }
            }
        }
        set.into_iter().collect()
    }

    pub fn list_tags(&self, include_archived: bool) -> Vec<String> {
        let g = self.inner.lock();
        let mut set = std::collections::BTreeSet::new();
        for s in g.sessions.values() {
            if !include_archived && s.archived {
                continue;
            }
            for t in &s.tags {
                if !t.is_empty() {
                    set.insert(t.clone());
                }
            }
        }
        set.into_iter().collect()
    }

    /// Returns the durable outcome rather than swallowing it: a caller that
    /// mutated a session must not report success when the process no longer
    /// holds durable-write authority for its home (#455).
    fn persist_session_meta_only(&self, id: Uuid) -> Result<()> {
        let write = self.durable_write("persisting session metadata")?;
        let session = {
            let g = self.inner.lock();
            match g.sessions.get(&id) {
                Some(s) => s.clone(),
                None => return Ok(()),
            }
        };
        session_store::save_session_meta(&write, &session)
    }

    pub fn fork_session(&self, source: Uuid) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(source)?;
        let summary = {
            let mut g = self.inner.lock();
            let src = g
                .sessions
                .get(&source)
                .ok_or_else(|| anyhow!("unknown session"))?
                .clone();
            let mut s = Session::new(src.cwd.clone(), src.model.clone(), src.effort);
            s.transcript = src.transcript.clone();
            s.transcript_loaded = true;
            s.persisted_len = 0;
            s.title = format!("{} (fork)", src.title);
            s.forked_from = Some(source);
            s.plan_mode = src.plan_mode;
            s.plan_steps = src.plan_steps.clone();
            s.plan_status = src.plan_status.clone();
            s.plan_goal = src.plan_goal.clone();
            s.compacted_summary = src.compacted_summary.clone();
            s.api_context_start = src.api_context_start;
            s.kind = src.kind;
            let summary = s.summary();
            g.active_session = Some(s.id);
            if !g.open_tab_ids.contains(&s.id) {
                g.open_tab_ids.push(s.id);
            }
            g.sessions.insert(s.id, s);
            summary
        };
        // Forked body is a full new log.
        self.persist_session_rewrite(summary.id);
        self.persist_chrome();
        Ok(summary)
    }

    /// Rewind conversation and/or restore files (#146).
    ///
    /// `mode`: `conversation` | `files` | `all` (default `conversation`).
    pub fn rewind_session(
        &self,
        id: Uuid,
        keep_messages: usize,
        mode: &str,
    ) -> Result<SessionSummary> {
        self.ensure_session_accepts_new_work(id)?;
        self.ensure_transcript_loaded(id)?;
        let mode = mode.trim().to_ascii_lowercase();
        let do_files = mode == "files" || mode == "all" || mode == "filesonly";
        let do_conv = mode != "files" && mode != "filesonly";

        if do_files {
            self.restore_edit_snapshots_for_session(id)?;
        }

        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            // Honor keep_messages for conversation modes (#146).
            // FilesOnly leaves transcript untouched.
            if do_conv && keep_messages < s.transcript.len() {
                s.transcript.truncate(keep_messages);
            }
            s.updated_at = Utc::now();
            s.summary()
        };
        if do_conv {
            self.persist_session_rewrite(id);
        }
        Ok(summary)
    }

    /// Snapshot original contents the first time a path is edited in this session (#146).
    pub fn snapshot_edit_original(&self, cwd: &Path, rel_path: &str) {
        let abs = cwd.join(rel_path);
        let key = abs.to_string_lossy().into_owned();
        let mut g = self.inner.lock();
        let sid = match g.active_session {
            Some(id) => id,
            None => return,
        };
        let map = g.edit_snapshots.entry(sid).or_default();
        if map.contains_key(&key) {
            return;
        }
        let original = std::fs::read_to_string(&abs).unwrap_or_default();
        map.insert(key, original);
    }

    /// Snapshot for an explicit session (tool path always knows the session).
    pub fn snapshot_edit_original_for_session(&self, session_id: Uuid, cwd: &Path, rel_path: &str) {
        let abs = cwd.join(rel_path);
        let key = abs.to_string_lossy().into_owned();
        let mut g = self.inner.lock();
        let map = g.edit_snapshots.entry(session_id).or_default();
        if map.contains_key(&key) {
            return;
        }
        let original = std::fs::read_to_string(&abs).unwrap_or_default();
        map.insert(key, original);
    }

    fn restore_edit_snapshots_for_session(&self, session_id: Uuid) -> Result<()> {
        let snaps: Vec<(String, String)> = {
            let g = self.inner.lock();
            g.edit_snapshots
                .get(&session_id)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
        for (path, content) in &snaps {
            let p = Path::new(path);
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if content.is_empty() && !p.exists() {
                continue;
            }
            if content.is_empty() {
                let _ = std::fs::remove_file(p);
            } else {
                std::fs::write(p, content)?;
            }
        }
        let mut g = self.inner.lock();
        g.edit_snapshots.remove(&session_id);
        // Drop edited_files entries that match restored paths (best-effort)
        let restored: std::collections::HashSet<_> = snaps.iter().map(|(p, _)| p.clone()).collect();
        g.edited_files
            .retain(|p| !restored.iter().any(|r| r.ends_with(p) || p.ends_with(r)));
        Ok(())
    }

    /// Shrink the *server-facing* context window only (sync extractive path).
    ///
    /// Local `transcript.jsonl` is never truncated or rewritten: every message
    /// stays on disk for search, UI, and perpetual history. Compact advances
    /// [`Session::api_context_start`] and stores a summary of the portion that
    /// leaves the API window in [`Session::compacted_summary`].
    pub fn compact_session(&self, id: Uuid) -> Result<SessionSummary> {
        self.compact_session_inner(id, None)
    }

    /// Compact with optional LLM-quality summary text for the leaving window.
    pub fn compact_session_with_summary(
        &self,
        id: Uuid,
        quality_summary: Option<String>,
    ) -> Result<SessionSummary> {
        self.compact_session_inner(id, quality_summary)
    }

    fn compact_session_inner(
        &self,
        id: Uuid,
        quality_summary: Option<String>,
    ) -> Result<SessionSummary> {
        let write = self.durable_write("compacting a session")?;
        self.ensure_session_accepts_new_work(id)?;
        self.ensure_transcript_loaded(id)?;
        const KEEP_RECENT: usize = 6;
        let (summary, leaving_for_memory) = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let len_before = s.transcript.len();
            let mut leaving_texts = Vec::new();
            if len_before > KEEP_RECENT {
                let new_start = len_before - KEEP_RECENT;
                let old_start = s.api_context_start.min(len_before);
                if new_start > old_start {
                    let leaving = &s.transcript[old_start..new_start];
                    leaving_texts = leaving
                        .iter()
                        .filter(|e| e.role == "user" || e.role == "assistant")
                        .map(|e| e.text.clone())
                        .collect();
                    let piece = quality_summary
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| build_compact_summary(leaving));
                    s.compacted_summary = Some(match s.compacted_summary.take() {
                        Some(prev) if !prev.is_empty() => format!("{prev}\n\n{piece}"),
                        _ => piece,
                    });
                    s.api_context_start = new_start;
                    let total = s.transcript.len();
                    let in_window = total - new_start;
                    // Additive local notice only — never deletes prior entries.
                    s.transcript.push(TranscriptEntry::system(format!(
                        "[context compacted for server: {in_window} recent messages stay in the API window; full local history retained ({total} messages before this notice)]"
                    )));
                    debug_assert!(s.transcript.len() >= len_before);
                }
            }
            s.updated_at = Utc::now();
            (s.summary(), leaving_texts)
        };
        let project_memory = self
            .memory_access_for_session(id)
            .map(|access| access.project_if_allowed());
        // Best-effort memory flush of key decisions from compacted window.
        for t in leaving_for_memory.iter().take(12) {
            let lower = t.to_ascii_lowercase();
            if lower.contains("always ")
                || lower.contains("decision:")
                || lower.contains("remember:")
                || lower.contains("prefer ")
            {
                let clip: String = t.chars().take(400).collect();
                if let Ok(Some(address)) = &project_memory {
                    let _ =
                        crate::memory::remember(&write, address, &clip, &["compact-flush".into()]);
                }
            }
        }
        self.persist_session(id);
        Ok(summary)
    }

    /// Async compact: model-backed summary when online, extractive offline.
    pub async fn compact_session_async(&self, id: Uuid) -> Result<SessionSummary> {
        self.ensure_session_accepts_new_work(id)?;
        self.ensure_transcript_loaded(id)?;
        const KEEP_RECENT: usize = 6;
        let (cwd, leaving, model) = {
            let g = self.inner.lock();
            let s = g
                .sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let len = s.transcript.len();
            if len <= KEEP_RECENT {
                drop(g);
                return self.compact_session(id);
            }
            let new_start = len - KEEP_RECENT;
            let old_start = s.api_context_start.min(len);
            if new_start <= old_start {
                drop(g);
                return self.compact_session(id);
            }
            let leaving = s.transcript[old_start..new_start].to_vec();
            (s.cwd.clone(), leaving, g.model.clone())
        };

        let quality = if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
            None
        } else if let Some(creds) = crate::auth_store::resolve_wire_credentials_for_model(&model)
            .map_err(anyhow::Error::msg)?
        {
            let blob = build_compact_summary(&leaving);
            let prompt = format!(
                "Summarize this coding-agent conversation for future turns. \
                 Preserve: user goals, decisions, file paths touched, failing tests, open TODOs. \
                 Be dense (≤600 words). Do not invent facts.\n\n{blob}"
            );
            let (call_allowed, usage_attempt) = match self.begin_provider_attempt(id).await {
                Ok(attempt) => (true, attempt),
                Err(error) if self.run_token_stop_before_request(id).is_some() => (false, None),
                Err(error) => return Err(error),
            };
            if !call_allowed {
                None
            } else {
                match call_xai_chat(
                    &creds,
                    &model,
                    &[("user".into(), prompt)],
                    None,
                    &cwd,
                    SessionKind::Build,
                )
                .await
                {
                    Ok(reply) => {
                        self.finish_provider_attempt(id, usage_attempt, reply.usage.as_ref())?;
                        (!reply.text.trim().is_empty())
                            .then(|| format!("LLM compact summary:\n{}", reply.text))
                    }
                    Err(_) => {
                        self.finish_provider_attempt(id, usage_attempt, None)?;
                        None
                    }
                }
            }
        } else {
            None
        };

        self.compact_session_with_summary(id, quality)
    }

    /// Export full local transcript (never truncated by compact) as text.
    pub fn export_transcript(&self, id: Uuid) -> Result<String> {
        self.ensure_transcript_loaded(id)?;
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        let mut out = format!(
            "# GrokPtah transcript export\n\
             session: {}\n\
             title: {}\n\
             cwd: {}\n\
             model: {}\n\
             messages: {}\n\
             api_context_start: {}\n\
             compacted_summary_chars: {}\n\n",
            s.id,
            s.title,
            s.cwd.display(),
            s.model,
            s.transcript.len(),
            s.api_context_start,
            s.compacted_summary.as_ref().map(|c| c.len()).unwrap_or(0)
        );
        for (i, e) in s.transcript.iter().enumerate() {
            out.push_str(&format!("## [{i}] {}\n{}\n\n", e.role, e.text));
        }
        Ok(out)
    }

    /// Compact metadata for tests / diagnostics (local length never shrinks on compact).
    pub fn compact_stats(&self, id: Uuid) -> Result<(usize, usize, Option<String>)> {
        self.ensure_transcript_loaded(id)?;
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        Ok((
            s.transcript.len(),
            s.api_context_start,
            s.compacted_summary.clone(),
        ))
    }

    /// Build the same OpenAI message list the coding agent would send (system +
    /// compacted summary + windowed history). Used by tests and diagnostics so
    /// offline paths can still assert wire context quality after `/compact`.
    pub fn wire_messages_preview(&self, id: Uuid) -> Result<Vec<serde_json::Value>> {
        self.ensure_transcript_loaded(id)?;
        let memory_access = self.memory_access_for_session(id)?;
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        let history = api_context_messages(s);
        let plan = if matches!(s.plan_status.as_str(), "accepted" | "executing" | "done")
            && !s.plan_steps.is_empty()
        {
            Some((
                s.plan_goal.as_deref().unwrap_or("execute plan"),
                s.plan_steps.as_slice(),
            ))
        } else {
            None
        };
        Ok(build_agent_messages(
            &history,
            s.compacted_summary.as_deref(),
            &s.cwd,
            Some(&memory_access),
            plan,
        ))
    }

    /// Test-only: mark the session busy then panic. [`TurnBusyGuard`] must clear
    /// busy on unwind so a follow-up turn is accepted.
    ///
    /// `#[doc(hidden)]` — for integration tests (not a product API).
    #[doc(hidden)]
    pub fn test_only_panic_while_turn_busy(&self, session_id: Uuid) {
        let cancel = CancellationToken::new();
        {
            let mut g = self.inner.lock();
            g.turn_cancels.insert(session_id, cancel);
        }
        assert!(self.session_busy(session_id));
        let _guard = TurnBusyGuard {
            host: self.clone(),
            session_id,
            armed: true,
        };
        panic!("simulated mid-turn panic");
    }

    /// Surface a rate-limit (or other) agent failure the same way a live turn
    /// does — emits [`SessionUpdate::RateLimited`] when appropriate plus a
    /// user-visible error chunk. Public for offline resilience tests.
    pub fn surface_agent_failure(&self, session_id: Uuid, err: &str) -> Result<()> {
        let event_tx = self.inner.lock().event_tx.clone();
        surface_rate_limit_or_error(&event_tx, session_id, err);
        Ok(())
    }

    /// Last agent-edited path for this process (for one-click diff UI).
    pub fn last_edited_path(&self) -> Option<String> {
        self.inner.lock().edited_files.last().cloned()
    }

    /// Resolve memory from the session's durable source workspace. Execution
    /// worktrees and focused desktop chrome are intentionally not consulted.
    fn memory_access_for_session(&self, session_id: Uuid) -> Result<MemoryAccess> {
        let session = {
            let g = self.inner.lock();
            g.sessions
                .get(&session_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown session"))?
        };
        let actor_agent = match session.agent_id.as_deref() {
            None => None,
            Some(agent_id) => {
                let agent = self
                    .ensure_orchestration_store()?
                    .load_agent(agent_id)?
                    .ok_or_else(|| {
                        anyhow!("session Agent binding has no durable Agent record: {agent_id}")
                    })?;
                if !agent.known_lane_ids().contains(&session_id) {
                    bail!(
                        "session Agent binding mismatch: Lane {session_id} is not owned by {agent_id}"
                    );
                }
                Some(agent.agent_id)
            }
        };
        let access = MemoryAccess::new(&session.cwd, actor_agent.clone());
        if actor_agent.is_none() {
            return Ok(access);
        }
        let spec = self
            .session_agent_spec(session_id)?
            .ok_or_else(|| anyhow!("persistent Agent specification is unavailable"))?;
        if spec.source_workspace != session.cwd.display().to_string() {
            bail!("persistent Agent memory scope does not match the Lane source workspace");
        }
        access.with_agent_policy(
            spec.memory.project_scope,
            spec.memory.agent_private_scope,
            spec.memory.team_ids,
        )
    }

    fn memory_address_for_session(
        &self,
        session_id: Uuid,
        scope: MemoryScope,
    ) -> Result<MemoryAddress> {
        self.memory_access_for_session(session_id)?.resolve(scope)
    }

    fn memory_address_from_args(
        &self,
        session_id: Uuid,
        args: &serde_json::Value,
    ) -> Result<MemoryAddress> {
        let scope = args
            .get("scope")
            .ok_or_else(|| anyhow!("memory tool requires an explicit scope"))?;
        let scope: MemoryScope = serde_json::from_value(scope.clone())
            .context("memory tool scope descriptor is invalid")?;
        self.memory_address_for_session(session_id, scope)
    }

    pub fn memory_list(
        &self,
        session_id: Uuid,
        scope: MemoryScope,
    ) -> Result<Vec<crate::memory::MemoryFact>> {
        let address = self.memory_address_for_session(session_id, scope)?;
        crate::memory::list_facts(&address)
    }

    pub fn memory_remember(
        &self,
        session_id: Uuid,
        scope: MemoryScope,
        text: &str,
    ) -> Result<String> {
        let write = self.durable_write("writing a memory fact")?;
        let address = self.memory_address_for_session(session_id, scope)?;
        crate::memory::remember(&write, &address, text, &[]).map_err(|error| anyhow!(error))
    }

    pub fn set_model(&self, model: String) {
        let changed = self.inner.lock().model != model;
        if changed {
            self.invalidate_computer_agent_authority();
        }
        self.inner.lock().model = model;
        self.persist_chrome();
    }

    /// Accumulate token usage for /usage (#159).
    async fn begin_provider_attempt(&self, session_id: Uuid) -> Result<Option<RunUsageAttempt>> {
        let tracker = self.run_usage_trackers.lock().get(&session_id).cloned();
        Self::begin_provider_attempt_for_tracker(tracker).await
    }

    async fn begin_provider_attempt_for_tracker(
        tracker: Option<Arc<RunUsageTracker>>,
    ) -> Result<Option<RunUsageAttempt>> {
        match tracker {
            Some(tracker) => Ok(Some(tracker.begin_attempt().await?)),
            None => Ok(None),
        }
    }

    fn finish_provider_attempt(
        &self,
        session_id: Uuid,
        attempt: Option<RunUsageAttempt>,
        usage: Option<&CompletionUsage>,
    ) -> Result<Option<String>> {
        if let Some(usage) = usage {
            self.record_session_usage(
                session_id,
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens,
            );
        }
        match attempt {
            Some(attempt) => attempt.finish(usage),
            None => Ok(None),
        }
    }

    /// Accumulate token usage for /usage (#159).
    fn record_provider_usage(
        &self,
        session_id: Uuid,
        usage: Option<&CompletionUsage>,
    ) -> Result<Option<String>> {
        let tracker = self.run_usage_trackers.lock().get(&session_id).cloned();
        self.record_provider_usage_for_tracker(session_id, usage, tracker.as_deref())
    }

    fn record_provider_usage_for_tracker(
        &self,
        session_id: Uuid,
        usage: Option<&CompletionUsage>,
        tracker: Option<&RunUsageTracker>,
    ) -> Result<Option<String>> {
        if let Some(usage) = usage {
            self.record_session_usage(
                session_id,
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens,
            );
        }
        match tracker {
            Some(tracker) => tracker.record(usage),
            None => Ok(None),
        }
    }

    fn run_token_stop_before_request(&self, session_id: Uuid) -> Option<String> {
        self.run_usage_trackers
            .lock()
            .get(&session_id)
            .and_then(|tracker| tracker.stop_message())
    }

    fn run_tokens_bounded(&self, session_id: Uuid) -> bool {
        self.run_usage_trackers
            .lock()
            .get(&session_id)
            .is_some_and(|tracker| tracker.is_bounded())
    }

    fn mark_run_stop(&self, session_id: Uuid, cause: RunStopCause, code: &str) -> Result<()> {
        if let Some(tracker) = self.run_usage_trackers.lock().get(&session_id).cloned() {
            tracker.mark_host_stop(cause, code)?;
        }
        Ok(())
    }

    /// Accumulate token usage for /usage (#159).
    pub fn record_session_usage(
        &self,
        session_id: Uuid,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    ) {
        let mut g = self.inner.lock();
        let u = g.session_usage.entry(session_id).or_default();
        u.prompt_tokens = u.prompt_tokens.saturating_add(prompt_tokens);
        u.completion_tokens = u.completion_tokens.saturating_add(completion_tokens);
        u.total_tokens = u.total_tokens.saturating_add(if total_tokens > 0 {
            total_tokens
        } else {
            prompt_tokens.saturating_add(completion_tokens)
        });
        u.requests = u.requests.saturating_add(1);
    }

    /// Snapshot session usage so a turn can report only its own delta.
    pub fn session_usage_snapshot(&self, session_id: Uuid) -> (u64, u64, u64, u64) {
        let g = self.inner.lock();
        g.session_usage
            .get(&session_id)
            .map(|u| {
                (
                    u.prompt_tokens,
                    u.completion_tokens,
                    u.total_tokens,
                    u.requests,
                )
            })
            .unwrap_or_default()
    }

    pub fn set_effort(&self, effort: EffortLevel) {
        self.inner.lock().effort = effort;
        self.persist_chrome();
    }

    /// Single source of truth for global tool prompting (#113).
    ///
    /// `true`  → always_approve + permission_mode=bypassPermissions  
    /// `false` → prompt mode
    pub fn set_always_approve(&self, v: bool) {
        let mut g = self.inner.lock();
        g.always_approve = v;
        g.permission_mode = if v {
            "bypassPermissions".into()
        } else {
            "default".into()
        };
        drop(g);
        self.persist_chrome();
    }

    pub fn set_sandbox(&self, profile: String) {
        self.inner.lock().sandbox_profile = normalize_sandbox_profile(&profile).to_string();
        self.persist_chrome();
    }

    pub fn set_subagent_isolation(&self, mode: String) -> Result<()> {
        let mode = SubagentIsolationPreference::parse(&mode)
            .ok_or_else(|| anyhow!("subagent isolation must be `worktree` or `shared`"))?;
        self.inner.lock().subagent_isolation = mode;
        self.persist_chrome();
        Ok(())
    }

    pub fn set_appearance(&self, appearance: String) {
        self.inner.lock().appearance = appearance;
        self.persist_chrome();
    }

    /// Keep permission_mode and always_approve as one coherent control (#113).
    pub fn set_permission_mode(&self, mode: String) {
        let mut g = self.inner.lock();
        let bypass = mode == "bypassPermissions" || mode == "bypass" || mode == "yolo";
        g.permission_mode = if bypass {
            "bypassPermissions".into()
        } else {
            "default".into()
        };
        g.always_approve = bypass;
        drop(g);
        self.persist_chrome();
    }

    pub fn set_allow_deny_rules(&self, allow: Vec<String>, deny: Vec<String>) {
        let mut g = self.inner.lock();
        g.allow_rules = allow;
        g.deny_rules = deny;
    }

    fn session_agent_spec(&self, session_id: Uuid) -> Result<Option<AgentSpec>> {
        let agent_id = {
            let g = self.inner.lock();
            g.sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?
                .agent_id
                .clone()
        };
        let Some(agent_id) = agent_id else {
            return Ok(None);
        };
        let store = self.orchestration_store.lock().clone();
        let store = store.ok_or_else(|| anyhow!("persistent Agent store is unavailable"))?;
        let agent = store
            .load_agent(&agent_id)?
            .ok_or_else(|| anyhow!("persistent Agent record is missing"))?;
        if let Some(run_id) = agent.current_run_id.as_deref() {
            let run = store
                .load_run(run_id)?
                .ok_or_else(|| anyhow!("persistent Agent active Run record is missing"))?;
            if run.agent_id.as_deref() != Some(agent_id.as_str()) {
                bail!("persistent Agent active Run identity is inconsistent");
            }
            if let Some(revision) = run.agent_spec_revision {
                return store
                    .load_agent_spec(&agent_id, revision)?
                    .map(Some)
                    .ok_or_else(|| anyhow!("active Run Agent specification revision is missing"));
            }
        }
        Ok(Some(agent.current_spec()?.clone()))
    }

    fn session_agent_authority(&self, session_id: Uuid) -> Result<Option<AgentAuthorityPolicy>> {
        Ok(self
            .session_agent_spec(session_id)?
            .map(|spec| spec.authority))
    }

    /// Manager reasoning Runs are proposal-only. The durable Run purpose is
    /// the authority; a missing or unreadable active record fails closed.
    fn session_run_is_manager_proposal(&self, session_id: Uuid) -> Result<bool> {
        let Some(run_id) = self.current_turn_run_id(session_id) else {
            return Ok(false);
        };
        let store = self
            .orchestration_store
            .lock()
            .clone()
            .ok_or_else(|| anyhow!("persistent Run store is unavailable"))?;
        let run = store
            .load_run(&run_id)?
            .ok_or_else(|| anyhow!("active Run record is missing"))?;
        if run.session_id != session_id {
            bail!("active Run session does not match the current turn");
        }
        Ok(run.purpose == RunPurpose::ManagerProposal)
    }

    /// Intersect mutable host policy with the Agent's captured ceiling. Either
    /// side may deny or require approval; auto-approval requires both.
    fn tool_gate(&self, session_id: Uuid, tool_name: &str) -> ToolGate {
        self.tool_gate_inner(session_id, tool_name, true)
    }

    fn tool_gate_inner(
        &self,
        session_id: Uuid,
        tool_name: &str,
        enforce_tool_allowlist: bool,
    ) -> ToolGate {
        match self.session_run_is_manager_proposal(session_id) {
            Ok(false) => {}
            Ok(true) | Err(_) => return ToolGate::AutoDeny,
        }
        let ambient = {
            let g = self.inner.lock();
            evaluate_tool_gate(
                tool_name,
                g.always_approve,
                &g.always_allowed_tools,
                &g.permission_mode,
                &g.allow_rules,
                &g.deny_rules,
            )
        };
        let captured = match self.session_agent_authority(session_id) {
            Ok(Some(policy)) => {
                if enforce_tool_allowlist
                    && !policy.allowed_tools.iter().any(|tool| tool == tool_name)
                {
                    return ToolGate::AutoDeny;
                }
                let allowed = policy
                    .auto_allowed_tools
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>();
                evaluate_tool_gate(
                    tool_name,
                    policy.bypass_permissions,
                    &allowed,
                    if policy.bypass_permissions {
                        "bypassPermissions"
                    } else {
                        "default"
                    },
                    &policy.allow_rules,
                    &policy.deny_rules,
                )
            }
            Ok(None) => return ambient,
            Err(_) => return ToolGate::AutoDeny,
        };
        match (ambient, captured) {
            (ToolGate::AutoDeny, _) | (_, ToolGate::AutoDeny) => ToolGate::AutoDeny,
            (ToolGate::AutoAllow, ToolGate::AutoAllow) => ToolGate::AutoAllow,
            _ => ToolGate::Prompt,
        }
    }

    fn session_sandbox_is_readonly(&self, session_id: Uuid) -> bool {
        let ambient = self.inner.lock().sandbox_profile.clone();
        sandbox_is_readonly(&ambient)
            || match self.session_agent_authority(session_id) {
                Ok(Some(policy)) => sandbox_is_readonly(&policy.sandbox_profile),
                Ok(None) => false,
                Err(_) => true,
            }
    }

    fn session_sandbox_blocks_shell(&self, session_id: Uuid, command: &str) -> bool {
        let ambient = self.inner.lock().sandbox_profile.clone();
        if sandbox_blocks_shell(&ambient, command) {
            return true;
        }
        match self.session_agent_authority(session_id) {
            Ok(Some(policy)) => sandbox_blocks_shell(&policy.sandbox_profile, command),
            Ok(None) => false,
            Err(_) => true,
        }
    }

    fn session_exec_risk_policy(&self, session_id: Uuid) -> (String, bool) {
        let (ambient_profile, ambient_bypass) = {
            let g = self.inner.lock();
            (g.sandbox_profile.clone(), g.always_approve)
        };
        match self.session_agent_authority(session_id) {
            Ok(Some(policy)) => {
                let profile = if sandbox_is_readonly(&ambient_profile)
                    || sandbox_is_readonly(&policy.sandbox_profile)
                {
                    "read-only".into()
                } else if ambient_profile == "workspace-write"
                    || policy.sandbox_profile == "workspace-write"
                {
                    "workspace-write".into()
                } else {
                    ambient_profile
                };
                (profile, ambient_bypass && policy.bypass_permissions)
            }
            Ok(None) => (ambient_profile, ambient_bypass),
            Err(_) => ("read-only".into(), false),
        }
    }

    #[cfg(test)]
    fn ambient_tool_gate(&self, tool_name: &str) -> ToolGate {
        let g = self.inner.lock();
        evaluate_tool_gate(
            tool_name,
            g.always_approve,
            &g.always_allowed_tools,
            &g.permission_mode,
            &g.allow_rules,
            &g.deny_rules,
        )
    }

    pub fn models(&self) -> Vec<ModelInfo> {
        let selected = self.inner.lock().model.clone();
        let catalog = crate::models_catalog::load_catalog();
        let xai_credential_selection = crate::gateway_config::parse_model_selection(&selected)
            .ok()
            .filter(|selection| selection.provider_id == crate::gateway_config::XAI_PROVIDER_ID)
            .map(|_| selected.clone())
            .or_else(|| catalog.first().map(|entry| entry.info.id.clone()));
        // Resolve once through the same live source used by execution so env,
        // keychain, and Grok Build session changes are reflected immediately.
        let xai_credentials = xai_credential_selection.as_deref().and_then(|selection| {
            crate::auth_store::resolve_wire_credentials_for_model(selection)
                .ok()
                .flatten()
        });
        let xai_oidc_token_auth = xai_credentials
            .as_ref()
            .is_some_and(|credentials| credentials.oidc_token_auth);
        let xai_credential_fingerprint = xai_credentials
            .as_ref()
            .map(crate::auth_store::WireCredentials::qualification_identity_fingerprint);
        let mut models: Vec<ModelInfo> = catalog
            .into_iter()
            .map(|catalog_model| {
                let mut info = catalog_model.info;
                let selection = crate::gateway_config::ModelSelection {
                    provider_id: crate::gateway_config::XAI_PROVIDER_ID.into(),
                    model_id: info.id.clone(),
                };
                if let Ok(profile) = crate::gateway_config::resolve_profile_for_selection(
                    &selection,
                    xai_oidc_token_auth,
                    xai_credential_fingerprint.as_deref(),
                ) {
                    if let Some(model) = profile.models.first() {
                        let capabilities = &model.capabilities;
                        info.wire_model_id = model.wire_model_id().to_string();
                        info.supports_tools = capabilities.tools;
                        info.supports_stream = capabilities.stream;
                        info.supports_image_input = capabilities.image_input;
                        info.computer_use_tier =
                            capabilities.effective_computer_use_tier().as_str().into();
                        info.computer_capability_source =
                            match capabilities.computer_capability_source {
                                crate::gateway_config::CapabilitySource::Declared => "declared",
                                crate::gateway_config::CapabilitySource::Measured => "measured",
                                crate::gateway_config::CapabilitySource::Unknown => "unknown",
                            }
                            .into();
                        info.capability_source = match capabilities.source {
                            crate::gateway_config::CapabilitySource::Declared => "declared",
                            crate::gateway_config::CapabilitySource::Measured => "measured",
                            crate::gateway_config::CapabilitySource::Unknown => "unknown",
                        }
                        .into();
                        info.supports_effort = !capabilities.effort_options.is_empty();
                        info.effort_options.clone_from(&capabilities.effort_options);
                    }
                }
                info
            })
            .collect();
        let selected = crate::gateway_config::parse_model_selection(&selected).ok();
        let config = crate::gateway_config::load();
        for profile in &config.profiles {
            let mut profile_models = profile.models.clone();
            if profile_models.is_empty() {
                if let Some(selection) = selected
                    .as_ref()
                    .filter(|selection| selection.provider_id == profile.id)
                {
                    let mut legacy = crate::gateway_config::ProviderModel::unqualified(
                        selection.model_id.clone(),
                    );
                    if config.has_pending_legacy_secret() || profile.managed_by_env {
                        legacy.capabilities.tools = true;
                        legacy.capabilities.stream = true;
                        legacy.capabilities.parallel_tool_calls = true;
                        legacy.capabilities.source =
                            crate::gateway_config::CapabilitySource::Declared;
                    }
                    profile_models.push(legacy);
                }
            }
            for provider_model in profile_models {
                let capabilities = &provider_model.capabilities;
                models.push(ModelInfo {
                    id: crate::gateway_config::model_selection_key(&profile.id, &provider_model.id),
                    display_name: format!("{} · {}", provider_model.display_name, profile.label),
                    provider_id: profile.id.clone(),
                    provider_label: profile.label.clone(),
                    wire_model_id: provider_model.id,
                    supports_tools: capabilities.tools,
                    supports_stream: capabilities.stream,
                    supports_image_input: capabilities.image_input,
                    computer_use_tier: capabilities.effective_computer_use_tier().as_str().into(),
                    computer_capability_source: match capabilities.computer_capability_source {
                        crate::gateway_config::CapabilitySource::Declared => "declared",
                        crate::gateway_config::CapabilitySource::Measured => "measured",
                        crate::gateway_config::CapabilitySource::Unknown => "unknown",
                    }
                    .into(),
                    capability_source: match capabilities.source {
                        crate::gateway_config::CapabilitySource::Declared => "declared",
                        crate::gateway_config::CapabilitySource::Measured => "measured",
                        crate::gateway_config::CapabilitySource::Unknown => "unknown",
                    }
                    .into(),
                    supports_effort: !capabilities.effort_options.is_empty(),
                    effort_options: capabilities.effort_options.clone(),
                });
            }
        }
        models
    }

    pub fn auth_state(&self) -> AuthState {
        // Refresh from keyring/env so external key changes are visible
        let state = crate::auth_store::load_auth_state();
        self.inner.lock().auth = state.clone();
        state
    }

    pub fn sign_in_local(&self, display_name: String) -> AuthState {
        // Local display-only session without API key (still marked signed-in for UI)
        let mut g = self.inner.lock();
        g.auth = AuthState {
            signed_in: true,
            display_name: Some(display_name),
            method: Some("local".into()),
        };
        g.auth.clone()
    }

    pub fn set_api_key(&self, api_key: String, display_name: String) -> Result<AuthState> {
        let write = self.durable_write("storing the API key")?;
        let state = crate::auth_store::store_api_key(&write, &api_key, &display_name)
            .map_err(|e| anyhow!(e))?;
        self.inner.lock().auth = state.clone();
        Ok(state)
    }

    pub fn open_login(&self) -> Result<String> {
        crate::auth_store::open_login_page().map_err(|e| anyhow!(e))
    }

    pub fn sign_out(&self) -> AuthState {
        let state = crate::auth_store::clear_credentials();
        self.inner.lock().auth = state.clone();
        state
    }

    pub fn mcp_list(&self) -> Vec<McpServerInfo> {
        let project = self.inner.lock().project_cwd.clone();
        let list = crate::discover::load_mcp_servers(project.as_deref());
        self.inner.lock().mcp_servers = list.clone();
        list
    }

    /// Project-local MCP trust status for the open project (or defaults).
    pub fn mcp_project_trust(&self) -> McpProjectTrust {
        let project = self.inner.lock().project_cwd.clone();
        match project {
            Some(p) => McpProjectTrust {
                project: Some(p.display().to_string()),
                has_local_mcp: crate::discover::project_has_local_mcp_servers(&p),
                trusted: crate::discover::is_project_mcp_trusted(&p),
                decided: crate::discover::project_mcp_trust_decided(&p),
            },
            None => McpProjectTrust {
                project: None,
                has_local_mcp: false,
                trusted: false,
                decided: false,
            },
        }
    }

    pub fn mcp_set_project_trust(&self, trusted: bool) -> Result<McpProjectTrust> {
        let project = self
            .inner
            .lock()
            .project_cwd
            .clone()
            .ok_or_else(|| anyhow!("no project open"))?;
        crate::discover::set_project_mcp_trusted(&project, trusted).map_err(|e| anyhow!(e))?;
        Ok(self.mcp_project_trust())
    }

    pub fn mcp_set_enabled(&self, name: &str, enabled: bool) -> Result<McpServerInfo> {
        let project = self.inner.lock().project_cwd.clone();
        if !crate::discover::save_mcp_server_enabled(project.as_deref(), name, enabled) {
            // still update in-memory for tests without config file write success
            let mut g = self.inner.lock();
            if let Some(s) = g.mcp_servers.iter_mut().find(|s| s.name == name) {
                s.enabled = enabled;
                s.status = if enabled {
                    "configured".into()
                } else {
                    "disabled".into()
                };
                return Ok(s.clone());
            }
            bail!("unknown MCP server");
        }
        let list = crate::discover::load_mcp_servers(project.as_deref());
        let mut g = self.inner.lock();
        g.mcp_servers = list;
        g.mcp_servers
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown MCP server"))
    }

    pub fn mcp_doctor(&self) -> Vec<String> {
        let project = self.inner.lock().project_cwd.clone();
        crate::discover::mcp_doctor_lines(project.as_deref())
    }

    pub fn mcp_add_stdio(&self, name: &str, command: &str, args: Vec<String>) -> Result<()> {
        crate::discover::add_mcp_stdio(name, command, args).map_err(|e| anyhow!(e))?;
        let project = self.inner.lock().project_cwd.clone();
        let list = crate::discover::load_mcp_servers(project.as_deref());
        self.inner.lock().mcp_servers = list;
        Ok(())
    }

    pub fn plugins(&self) -> Vec<PluginInfo> {
        let list = crate::discover::discover_plugins();
        self.inner.lock().plugins = list.clone();
        list
    }

    pub fn plugin_install(&self, id: &str) -> Result<PluginInfo> {
        let p = crate::discover::install_plugin(id).map_err(|e| anyhow!(e))?;
        self.inner.lock().plugins = crate::discover::discover_plugins();
        Ok(p)
    }

    pub fn skills(&self) -> Vec<SkillInfo> {
        let project = self.inner.lock().project_cwd.clone();
        let list = crate::discover::discover_skills(project.as_deref());
        self.inner.lock().skills = list.clone();
        list
    }

    pub fn hooks_config(&self) -> String {
        let project = self.inner.lock().project_cwd.clone();
        crate::discover::hooks_config_text(project.as_deref())
    }

    /// #164 agent definitions from `.grok/agents` / `.grokptah/agents`.
    pub fn list_agents(&self) -> Vec<crate::agents_personas::AgentDef> {
        let project = self.inner.lock().project_cwd.clone();
        crate::agents_personas::discover_agents(project.as_deref())
    }

    /// #164 personas from `.grok/personas` / `.grokptah/personas`.
    pub fn list_personas(&self) -> Vec<crate::agents_personas::PersonaDef> {
        let project = self.inner.lock().project_cwd.clone();
        crate::agents_personas::discover_personas(project.as_deref())
    }

    /// #165 accurate count of running subagents for a session (or all).
    pub fn running_subagent_count(&self, session_id: Option<Uuid>) -> usize {
        let g = self.inner.lock();
        g.subagents
            .iter()
            .filter(|s| s.status == "running")
            .filter(|s| match session_id {
                None => true,
                Some(want) => {
                    s.session_id
                        .as_ref()
                        .and_then(|sid| Uuid::parse_str(sid).ok())
                        == Some(want)
                }
            })
            .count()
    }

    /// #174 fleet observability snapshot (usage + running subagents per session).
    pub fn fleet_observability(&self) -> serde_json::Value {
        let g = self.inner.lock();
        let mut sessions = Vec::new();
        for (id, s) in &g.sessions {
            let running = g
                .subagents
                .iter()
                .filter(|a| {
                    a.status == "running"
                        && a.session_id.as_deref() == Some(id.to_string().as_str())
                })
                .count();
            let usage = g.session_usage.get(id).cloned().unwrap_or_default();
            sessions.push(serde_json::json!({
                "session_id": id.to_string(),
                "title": s.title,
                "busy": g.turn_cancels.contains_key(id),
                "running_subagents": running,
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
                "usage_requests": usage.requests,
            }));
        }
        let running_total = g.subagents.iter().filter(|s| s.status == "running").count();
        serde_json::json!({
            "running_subagents_total": running_total,
            "sessions": sessions,
        })
    }

    pub fn agent_edit_diffs(&self) -> Result<String> {
        let (cwd, files) = {
            let g = self.inner.lock();
            (
                g.project_cwd
                    .clone()
                    .ok_or_else(|| anyhow!("no project open"))?,
                g.edited_files.clone(),
            )
        };
        if files.is_empty() {
            // fall back to full git diff
            return self.git_diff();
        }
        let mut out = String::new();
        for f in files {
            let output = std::process::Command::new("git")
                .args(["diff", "HEAD", "--", &f])
                .current_dir(&cwd)
                .output()?;
            out.push_str(&format!("--- {f} ---\n"));
            out.push_str(&String::from_utf8_lossy(&output.stdout));
            out.push('\n');
        }
        Ok(out)
    }

    pub fn record_edit(&self, path: &str) {
        let mut g = self.inner.lock();
        if !g.edited_files.iter().any(|p| p == path) {
            g.edited_files.push(path.to_string());
        }
    }

    /// Replace the most recent assistant transcript entry (used when evidence-
    /// backed handoff enrichment extends a weak model final).
    fn replace_last_assistant_text(&self, session_id: Uuid, text: &str) {
        let mut g = self.inner.lock();
        if let Some(session) = g.sessions.get_mut(&session_id) {
            if let Some(entry) = session
                .transcript
                .iter_mut()
                .rev()
                .find(|entry| entry.role == "assistant")
            {
                entry.text = text.to_string();
            } else {
                session.transcript.push(TranscriptEntry::assistant(text));
            }
        }
    }

    pub fn subagents(&self) -> Vec<SubagentInfo> {
        self.inner.lock().subagents.clone()
    }

    /// A bounded Run cannot finalize while a fire-and-forget child can still
    /// spend against it. Cancel only this Lane's running children and wait for
    /// their in-flight provider reads to settle before freezing the ledger.
    async fn quiesce_bounded_run_subagents(
        &self,
        session_id: Uuid,
        tracker: &RunUsageTracker,
    ) -> Result<()> {
        if !tracker.is_bounded() {
            return Ok(());
        }
        let session_key = session_id.to_string();
        let tokens = {
            let g = self.inner.lock();
            g.subagents
                .iter()
                .filter(|subagent| {
                    subagent.status == "running"
                        && subagent.session_id.as_deref() == Some(session_key.as_str())
                })
                .filter_map(|subagent| g.subagent_cancels.get(&subagent.id).cloned())
                .collect::<Vec<_>>()
        };
        for token in tokens {
            token.cancel();
        }
        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while self.running_subagent_count(Some(session_id)) > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        if !settled {
            // Spending may still be in flight, so an exact bounded total is no
            // longer provable. Persist the fail-closed state before finalizing.
            tracker.record(None)?;
        }
        Ok(())
    }

    /// Public spawn entry for tools, Tauri, and tests (#151).
    ///
    /// Starts the child on a background task and returns immediately so multiple
    /// children overlap. When a parent turn is active, child cancel is linked to
    /// that turn's token.
    pub async fn spawn_subagent_public(
        &self,
        session_id: Uuid,
        kind: &str,
        prompt: &str,
    ) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.sessions
                .get(&session_id)
                .map(|s| s.cwd.clone())
                .or_else(|| g.project_cwd.clone())
                .ok_or_else(|| anyhow!("no project/session cwd"))?
        };
        let parent_cancel = {
            let g = self.inner.lock();
            g.turn_cancels
                .get(&session_id)
                .cloned()
                .unwrap_or_else(CancellationToken::new)
        };
        let event_tx = self.inner.lock().event_tx.clone();
        self.spawn_gp_subagent_parallel(session_id, &cwd, prompt, kind, &parent_cancel, &event_tx)
    }

    /// Test helper: register a Computer Use operation the same way the
    /// production qualify/propose paths do, so lifecycle tests can assert that
    /// ordered shutdown cancels Computer authority without a live provider.
    pub fn begin_computer_agent_operation_for_test(
        &self,
        session_id: Uuid,
    ) -> Result<(String, CancellationToken)> {
        let (operation_id, cancel, guard) = self.begin_computer_agent_operation(session_id)?;
        // Deliberately leak the busy guard: this models the exact hazard #455
        // is about — a guard that still holds session authority when shutdown
        // starts. Ordered shutdown, not the guard's `Drop`, must be what
        // clears the registration and cancels the operation.
        std::mem::forget(guard);
        Ok((operation_id, cancel))
    }

    /// Number of Computer Use operations currently holding session authority.
    pub fn computer_agent_operation_count(&self) -> usize {
        self.inner.lock().computer_agent_operations.len()
    }

    /// Test helper: register a parent turn cancel token for `session_id`.
    pub fn begin_turn_for_test(&self, session_id: Uuid) {
        let mut g = self.inner.lock();
        g.turn_cancels.entry(session_id).or_default();
        g.begin_turn_generation(session_id);
    }

    /// Cancel a single subagent without cancelling the parent turn or siblings (#152).
    pub fn cancel_subagent(&self, id: &str) -> Result<()> {
        let write = self.durable_write("cancelling a subagent")?;
        let mut g = self.inner.lock();
        if let Some(token) = g.subagent_cancels.remove(id) {
            token.cancel();
        } else {
            // Still mark status if present.
        }
        let s = g
            .subagents
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow!("unknown subagent {id}"))?;
        if s.status == "running" {
            s.status = "cancelled".into();
            s.summary = Some("cancelled by user".into());
        }
        let session_id = s.session_id.as_ref().and_then(|x| Uuid::parse_str(x).ok());
        let snap = g.subagents.clone();
        drop(g);
        if let Some(sid) = session_id {
            let _ = session_store::save_session_subagents(&write, sid, &snap);
            let tx = self.inner.lock().event_tx.clone();
            let _ = tx.send(SessionUpdate::SubagentUpdate {
                session_id: sid,
                subagent_id: id.to_string(),
                status: "cancelled".into(),
                detail: Some("cancelled by user".into()),
            });
        }
        Ok(())
    }

    /// Load durable subagent history for a session into the live list (#152).
    pub fn load_session_subagents(&self, session_id: Uuid) {
        let hist = session_store::load_session_subagents(session_id);
        if hist.is_empty() {
            return;
        }
        let mut g = self.inner.lock();
        // Drop stale rows for this session; keep other sessions' live rows.
        g.subagents
            .retain(|s| s.session_id.as_deref() != Some(&session_id.to_string()));
        g.subagents.extend(hist);
    }

    pub fn background_tasks(&self) -> Vec<BackgroundTask> {
        self.inner.lock().background_tasks.clone()
    }

    pub fn cancel_background_task(&self, id: &str) -> Result<()> {
        // Agent tool shells: cancel the owning turn (kills live_shells) (#52).
        let session_for_shell = {
            let g = self.inner.lock();
            g.background_tasks
                .iter()
                .find(|t| t.id == id)
                .and_then(|t| t.session_id.clone())
        };
        if id.starts_with("shell-") {
            if let Some(sid) = session_for_shell.as_deref() {
                if let Ok(uuid) = Uuid::parse_str(sid) {
                    let _ = self.cancel_turn(Some(uuid));
                }
            }
        }
        let mut g = self.inner.lock();
        if let Some(token) = g.background_cancels.remove(id) {
            token.cancel();
        }
        let t = g
            .background_tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("unknown task"))?;
        t.status = "cancelled".into();
        t.detail = Some("cancelled by user".into());
        Ok(())
    }

    /// Schedule long-running work visible outside the transcript (#52).
    ///
    /// - Title starting with `!` runs a shell command in the project cwd.
    /// - Otherwise runs a cancellable project file scan with progress.
    pub fn schedule_background_task(&self, title: String) -> BackgroundTask {
        let id = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let is_shell = title.trim_start().starts_with('!');
        let t = BackgroundTask {
            id: id.clone(),
            title: title.clone(),
            status: "running".into(),
            scheduled: true,
            kind: if is_shell {
                "shell".into()
            } else {
                "scan".into()
            },
            session_id: self.inner.lock().active_session.map(|u| u.to_string()),
            detail: Some("starting…".into()),
        };
        {
            let mut g = self.inner.lock();
            g.background_tasks.push(t.clone());
            g.background_cancels.insert(id.clone(), cancel.clone());
        }
        let host = self.clone();
        let task_id = id.clone();
        let event_tx = self.inner.lock().event_tx.clone();
        let title_for_task = title.clone();
        // Background scans and shells hold host authority, so shutdown must be
        // able to cancel and join them (#455).
        let shutdown_cancel = cancel.clone();
        let shutdown = self.shutdown_token();
        let cascade =
            self.spawn_supervised("cascading shutdown to a background task", async move {
                shutdown.cancelled().await;
                shutdown_cancel.cancel();
            });
        drop(cascade);
        let spawned = self.spawn_supervised("scheduling a background task", async move {
            let final_status = if is_shell {
                let cmd = title_for_task.trim_start().trim_start_matches('!').trim();
                let cwd = host.inner.lock().project_cwd.clone();
                let cancel_c = cancel.clone();
                let cmd_owned = cmd.to_string();
                let run = async {
                    let mut command = tokio::process::Command::new("sh");
                    command.arg("-lc").arg(&cmd_owned);
                    if let Some(ref c) = cwd {
                        command.current_dir(c);
                    }
                    command.stdout(std::process::Stdio::piped());
                    command.stderr(std::process::Stdio::piped());
                    crate::spawn_env::scrub_tokio_command(&mut command);
                    let mut child = command.spawn().map_err(|e| e.to_string())?;
                    let mut stdout = child.stdout.take();
                    let mut stderr = child.stderr.take();
                    tokio::select! {
                        _ = cancel_c.cancelled() => {
                            let _ = child.start_kill();
                            let _ = child.wait().await;
                            Err("cancelled".to_string())
                        }
                        status = child.wait() => {
                            use tokio::io::AsyncReadExt;
                            let status = status.map_err(|e| e.to_string())?;
                            let mut body = String::new();
                            if let Some(ref mut out) = stdout {
                                let mut buf = Vec::new();
                                let _ = out.read_to_end(&mut buf).await;
                                body.push_str(&String::from_utf8_lossy(&buf));
                            }
                            if let Some(ref mut err) = stderr {
                                let mut buf = Vec::new();
                                let _ = err.read_to_end(&mut buf).await;
                                body.push_str(&String::from_utf8_lossy(&buf));
                            }
                            let clip: String = body.chars().take(400).collect();
                            if status.success() {
                                Ok(format!("completed · exit 0 · {clip}"))
                            } else {
                                Ok(format!("failed · {clip}"))
                            }
                        }
                    }
                };
                match run.await {
                    Ok(s) => s,
                    Err(e) => e,
                }
            } else {
                // Cancellable project scan (real walk with cancel polling).
                let cwd = host.inner.lock().project_cwd.clone();
                if let Some(cwd) = cwd {
                    let cancel_c = cancel.clone();
                    let host_p = host.clone();
                    let task_id_p = task_id.clone();
                    let title_p = title_for_task.clone();
                    let event_tx_p = event_tx.clone();
                    let walk = tokio::task::spawn_blocking(move || {
                        let mut n = 0usize;
                        for e in walkdir::WalkDir::new(cwd)
                            .max_depth(8)
                            .into_iter()
                            .flatten()
                        {
                            if cancel_c.is_cancelled() {
                                return Err(n);
                            }
                            if e.file_type().is_file() {
                                n += 1;
                                if n.is_multiple_of(250) {
                                    let mut g = host_p.inner.lock();
                                    if let Some(task) =
                                        g.background_tasks.iter_mut().find(|t| t.id == task_id_p)
                                    {
                                        if task.status != "cancelled" {
                                            task.detail = Some(format!("scanned {n} files…"));
                                        }
                                    }
                                    let _ = event_tx_p.send(SessionUpdate::BackgroundTask {
                                        session_id: None,
                                        task_id: task_id_p.clone(),
                                        title: title_p.clone(),
                                        status: format!("running ({n} files)"),
                                    });
                                }
                            }
                        }
                        Ok(n)
                    });
                    match walk.await {
                        Ok(Ok(n)) => format!("completed ({n} files)"),
                        Ok(Err(n)) => format!("cancelled after {n} files"),
                        Err(_) => "failed".into(),
                    }
                } else {
                    tokio::select! {
                        _ = cancel.cancelled() => "cancelled".to_string(),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {
                            "completed (no project open)".into()
                        }
                    }
                }
            };
            {
                let mut g = host.inner.lock();
                g.background_cancels.remove(&task_id);
                if let Some(task) = g.background_tasks.iter_mut().find(|t| t.id == task_id) {
                    if task.status != "cancelled" {
                        task.status = final_status.clone();
                        task.detail = Some(final_status.clone());
                    }
                }
            }
            let _ = event_tx.send(SessionUpdate::BackgroundTask {
                session_id: None,
                task_id: task_id.clone(),
                title: title_for_task,
                status: final_status,
            });
        });
        if spawned.is_err() {
            // Shutting down: never leave a phantom "running" row behind.
            let mut g = self.inner.lock();
            g.background_cancels.remove(&id);
            if let Some(task) = g.background_tasks.iter_mut().find(|task| task.id == id) {
                task.status = "cancelled".into();
                task.detail = Some("host shutdown".into());
            }
            let mut refused = t.clone();
            refused.status = "cancelled".into();
            refused.detail = Some("host shutdown".into());
            return refused;
        }
        t
    }

    /// Register a long-running agent shell as a background task (visible in Tasks panel).
    pub fn register_shell_background_task(
        &self,
        call_id: &str,
        command: &str,
        session_id: Option<Uuid>,
    ) {
        let t = BackgroundTask {
            id: format!("shell-{call_id}"),
            title: command.chars().take(80).collect(),
            status: "running".into(),
            scheduled: false,
            kind: "shell".into(),
            session_id: session_id.map(|u| u.to_string()),
            detail: Some("agent tool shell".into()),
        };
        let event_tx = {
            let mut g = self.inner.lock();
            // Replace prior entry for same call id.
            g.background_tasks.retain(|x| x.id != t.id);
            g.background_tasks.push(t.clone());
            g.event_tx.clone()
        };
        let _ = event_tx.send(SessionUpdate::BackgroundTask {
            session_id,
            task_id: t.id.clone(),
            title: t.title.clone(),
            status: t.status.clone(),
        });
    }

    pub fn complete_shell_background_task(&self, call_id: &str, status: &str) {
        let id = format!("shell-{call_id}");
        let (event_tx, title) = {
            let mut g = self.inner.lock();
            let title = if let Some(task) = g.background_tasks.iter_mut().find(|t| t.id == id) {
                task.status = status.into();
                task.detail = Some(status.into());
                task.title.clone()
            } else {
                return;
            };
            (g.event_tx.clone(), title)
        };
        let _ = event_tx.send(SessionUpdate::BackgroundTask {
            session_id: None,
            task_id: id,
            title,
            status: status.into(),
        });
    }

    pub fn fuzzy_open(&self, query: &str) -> Result<Vec<String>> {
        let g = self.inner.lock();
        let cwd = g
            .project_cwd
            .as_ref()
            .ok_or_else(|| anyhow!("no project open"))?;
        Ok(local_tools::fuzzy_files(cwd, query, 40))
    }

    pub fn file_tree(&self) -> Result<Vec<String>> {
        let g = self.inner.lock();
        let cwd = g
            .project_cwd
            .as_ref()
            .ok_or_else(|| anyhow!("no project open"))?;
        Ok(local_tools::list_tree(cwd, 200))
    }

    pub fn git_status(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["status", "--short", "--branch"])
            .current_dir(&cwd)
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn git_diff(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(&cwd)
            .output()?;
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(if s.len() > 64_000 {
            crate::textutil::truncate_with_marker(&s, 64_000, "\n…")
        } else {
            s
        })
    }

    pub fn git_stage_all(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&cwd)
            .output()?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr));
        }
        Ok("staged".into())
    }

    pub fn git_commit(&self, message: &str) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&cwd)
            .output()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            bail!("{text}");
        }
        Ok(text)
    }

    pub fn list_worktrees(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["worktree", "list"])
            .current_dir(&cwd)
            .output()?;
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        // #166: dry-run age GC report for managed isolation worktrees
        let managed = cwd.join(".grokptah").join("worktrees");
        if managed.is_dir() {
            let protected = self.active_managed_worktrees(&cwd);
            let report = crate::worktree_gc::gc_worktrees_with_protected(
                &managed,
                crate::worktree_gc::DEFAULT_MAX_AGE,
                true,
                &protected,
            );
            if report.scanned > 0 {
                s.push_str(&format!(
                    "\n# auto-gc dry-run: {} aged under .grokptah/worktrees (set GROKPTAH_WORKTREE_GC=1 to delete)\n",
                    report.scanned
                ));
            }
            if std::env::var_os("GROKPTAH_WORKTREE_GC").is_some() {
                let live = crate::worktree_gc::gc_worktrees_with_protected(
                    &managed,
                    crate::worktree_gc::DEFAULT_MAX_AGE,
                    false,
                    &protected,
                );
                s.push_str(&format!("# auto-gc removed {} paths\n", live.removed.len()));
            }
        }
        Ok(s)
    }

    fn active_managed_worktrees(&self, project: &Path) -> Vec<PathBuf> {
        let managed = dunce::canonicalize(project.join(".grokptah").join("worktrees"));
        let Ok(managed) = managed else {
            return Vec::new();
        };
        let mut protected = Vec::new();
        if let Ok(store) = self.ensure_orchestration_store() {
            if let Ok(runs) = store.list_runs() {
                for run in runs {
                    if run.state.is_terminal() {
                        continue;
                    }
                    if let Some(execution) = run.execution {
                        protected.push(PathBuf::from(execution.execution_workspace));
                    }
                }
            }
        }
        let g = self.inner.lock();
        protected.extend(
            g.subagents
                .iter()
                .filter(|subagent| subagent.status == "running")
                .filter_map(|subagent| subagent.cwd.as_ref().map(PathBuf::from)),
        );
        protected
            .into_iter()
            .filter_map(|path| dunce::canonicalize(path).ok())
            .filter(|path| path.starts_with(&managed))
            .collect()
    }

    /// Create a git worktree under the open project (#43).
    /// `path` is relative to the project root (or absolute). `branch` is optional
    /// (new branch `-b` when provided; otherwise checkout default HEAD).
    pub fn create_worktree(&self, path: &str, branch: Option<&str>) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let path = path.trim();
        if path.is_empty() {
            bail!("worktree path is required");
        }
        let target = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            cwd.join(path)
        };
        let mut cmd = std::process::Command::new("git");
        cmd.arg("worktree").arg("add").current_dir(&cwd);
        if let Some(b) = branch.map(str::trim).filter(|b| !b.is_empty()) {
            cmd.arg("-b").arg(b);
        }
        cmd.arg(&target);
        let out = cmd.output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            bail!(
                "git worktree add failed: {}",
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            );
        }
        Ok(format!(
            "Created worktree at {}\n{}{}",
            target.display(),
            stdout,
            stderr
        ))
    }

    /// Remove a worktree path (does not delete the branch).
    pub fn remove_worktree(&self, path: &str) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let path = path.trim();
        if path.is_empty() {
            bail!("worktree path is required");
        }
        let out = std::process::Command::new("git")
            .args(["worktree", "remove", "--force", path])
            .current_dir(&cwd)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            bail!(
                "git worktree remove failed: {}",
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            );
        }
        Ok(format!("{}{}", stdout, stderr))
    }

    pub fn project_rules(&self) -> Result<Vec<String>> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let candidates = [
            "AGENTS.md",
            "Claude.md",
            "CLAUDE.md",
            ".grok/rules.md",
            "docs/ARCHITECTURE.md",
        ];
        let mut found = Vec::new();
        for c in candidates {
            if cwd.join(c).is_file() {
                found.push(c.to_string());
            }
        }
        Ok(found)
    }

    pub fn settings_snapshot(&self) -> serde_json::Value {
        // Reconcile legacy dual-control drift so UI never shows conflicting state (#113).
        {
            let mut g = self.inner.lock();
            let bypass = g.always_approve || g.permission_mode == "bypassPermissions";
            g.always_approve = bypass;
            g.permission_mode = if bypass {
                "bypassPermissions".into()
            } else {
                "default".into()
            };
        }
        let g = self.inner.lock();
        let gw = crate::gateway_config::load();
        let active_gateway = gw
            .active_profile_id
            .as_deref()
            .and_then(|id| gw.profile(id))
            .or_else(|| gw.profiles.first());
        let (subagent_isolation, subagent_isolation_managed_by_env) =
            effective_subagent_isolation(g.subagent_isolation);
        serde_json::json!({
            "model": g.model,
            "effort": g.effort,
            "alwaysApprove": g.always_approve,
            "sandboxProfile": g.sandbox_profile,
            "subagentIsolation": subagent_isolation.as_str(),
            "subagentIsolationConfigured": g.subagent_isolation.as_str(),
            "subagentIsolationManagedByEnv": subagent_isolation_managed_by_env,
            "appearance": g.appearance,
            // Single effective mode for UI (mirrors alwaysApprove).
            "permissionMode": g.permission_mode,
            "effectiveToolPrompting": if g.always_approve { "bypass" } else { "prompt" },
            "allowRules": g.allow_rules,
            "denyRules": g.deny_rules,
            "autoUpdateEnabled": crate::desktop_auto_update_enabled(),
            // Corporate gateway (#169) — env overrides still win at resolve time.
            "gatewayProviderId": active_gateway.map(|profile| profile.id.as_str()).unwrap_or(""),
            "gatewayBaseUrl": active_gateway.map(|profile| profile.base_url.as_str()).unwrap_or(""),
            "gatewayApiKeySet": active_gateway
                .is_some_and(|profile| {
                    profile.credential_ref.as_deref().is_some_and(|reference| {
                        crate::auth_store::provider_credential_is_set(
                            &profile.id,
                            profile.managed_by_env,
                            reference,
                        )
                    })
                })
                || gw.has_pending_legacy_secret(),
            "gatewayProfiles": gw.profiles.iter().map(|profile| serde_json::json!({
                "id": profile.id,
                "label": profile.label,
                "baseUrl": profile.base_url,
                "deadlineClass": profile.deadline_class,
                "credentialSet": profile.credential_ref.as_deref()
                    .is_some_and(|reference| crate::auth_store::provider_credential_is_set(
                        &profile.id,
                        profile.managed_by_env,
                        reference,
                    ))
                    || (gw.has_pending_legacy_secret()
                        && gw.active_profile_id.as_deref() == Some(profile.id.as_str())),
                "managedByEnv": profile.managed_by_env,
                "models": profile.models.iter().map(|model| serde_json::json!({
                    "id": model.id,
                    "displayName": model.display_name,
                    "supportsTools": model.capabilities.tools,
                    "supportsStream": model.capabilities.stream,
                    "supportsImageInput": model.capabilities.image_input,
                    "computerUseTier": model.capabilities.effective_computer_use_tier(),
                    "computerCapabilitySource": model.capabilities.computer_capability_source,
                    "effortOptions": model.capabilities.effort_options,
                    "capabilitySource": model.capabilities.source,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "gatewayMigrationPending": gw.has_pending_legacy_secret(),
        })
    }

    /// Persist OpenAI-compatible gateway settings (#169). Empty strings clear fields.
    pub fn set_gateway_config(
        &self,
        provider_id: String,
        base_url: String,
        api_key: Option<String>,
    ) -> Result<()> {
        let write = self.durable_write("setting the gateway config")?;
        let provider_id = if provider_id.trim().is_empty() {
            "corporate".to_string()
        } else {
            crate::gateway_config::normalized_profile_id(&provider_id)
                .map_err(anyhow::Error::msg)?
        };
        crate::gateway_config::validate_base_url(&base_url).map_err(anyhow::Error::msg)?;
        let mut cfg = crate::gateway_config::load_for_update().context("read provider profiles")?;
        let current_selection = {
            let g = self.inner.lock();
            crate::gateway_config::parse_model_selection(&g.model).ok()
        };
        let current_model_id = current_selection
            .as_ref()
            .filter(|selection| {
                selection.provider_id == provider_id
                    || selection.provider_id == crate::gateway_config::XAI_PROVIDER_ID
            })
            .map(|selection| selection.model_id.clone())
            .unwrap_or_else(|| "model-id".into());
        let mut profile = cfg.profile(&provider_id).cloned().unwrap_or_else(|| {
            crate::gateway_config::ProviderProfile::openai_compatible(
                provider_id.clone(),
                provider_id.clone(),
                base_url.clone(),
            )
        });
        profile.set_base_url(&base_url);
        if !profile
            .models
            .iter()
            .any(|model| model.id == current_model_id)
        {
            profile.upsert_model(crate::gateway_config::ProviderModel::unqualified(
                current_model_id.clone(),
            ));
        }
        if let Some(key) = api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            profile.credential_ref = Some(
                crate::auth_store::store_provider_api_key(&write, &provider_id, key)
                    .map_err(anyhow::Error::msg)?,
            );
            cfg.clear_legacy_fields();
        } else if cfg.has_pending_legacy_secret() {
            bail!("legacy gateway credential must be migrated or replaced before saving");
        }
        cfg.upsert_profile(profile).map_err(anyhow::Error::msg)?;
        cfg.active_profile_id = Some(provider_id.clone());
        crate::gateway_config::save(&write, &cfg).map_err(|e| anyhow!("save gateway.json: {e}"))?;
        self.invalidate_computer_agent_authority();
        self.set_model(crate::gateway_config::model_selection_key(
            &provider_id,
            &current_model_id,
        ));
        self.set_effort(EffortLevel::None);
        Ok(())
    }

    pub fn upsert_provider_profile(
        &self,
        update: crate::gateway_config::ProviderProfileUpdate,
    ) -> Result<()> {
        let write = self.durable_write("upserting a provider profile")?;
        let crate::gateway_config::ProviderProfileUpdate {
            provider_id,
            label,
            base_url,
            model_id,
            deadline_class,
            effort_options,
            api_key,
        } = update;
        let provider_id = crate::gateway_config::normalized_profile_id(&provider_id)
            .map_err(anyhow::Error::msg)?;
        crate::gateway_config::validate_base_url(&base_url).map_err(anyhow::Error::msg)?;
        if model_id.trim().is_empty() {
            bail!("model id is required; use Discover or enter the gateway's exact id");
        }
        let mut config =
            crate::gateway_config::load_for_update().context("read provider profiles")?;
        if config.has_pending_legacy_secret()
            && config.active_profile_id.as_deref() != Some(provider_id.as_str())
        {
            bail!("migrate or remove the legacy gateway profile before adding another profile");
        }
        let mut profile = config.profile(&provider_id).cloned().unwrap_or_else(|| {
            crate::gateway_config::ProviderProfile::openai_compatible(
                provider_id.clone(),
                label.clone(),
                base_url.clone(),
            )
        });
        profile.label = label.trim().to_string();
        if profile.label.is_empty() {
            profile.label = provider_id.clone();
        }
        profile.set_base_url(&base_url);
        profile.deadline_class = deadline_class;
        if let Some(model) = profile.models.iter_mut().find(|model| model.id == model_id) {
            model.capabilities.effort_options = effort_options.clone();
            if !model.capabilities.effort_options.is_empty()
                && model.capabilities.source == crate::gateway_config::CapabilitySource::Unknown
            {
                model.capabilities.source = crate::gateway_config::CapabilitySource::Declared;
            }
        } else {
            let mut model = crate::gateway_config::ProviderModel::unqualified(&model_id);
            model.capabilities.effort_options = effort_options.clone();
            if !model.capabilities.effort_options.is_empty() {
                model.capabilities.source = crate::gateway_config::CapabilitySource::Declared;
            }
            profile.upsert_model(model);
        }
        if let Some(key) = api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            profile.credential_ref = Some(
                crate::auth_store::store_provider_api_key(&write, &provider_id, key)
                    .map_err(anyhow::Error::msg)?,
            );
            config.clear_legacy_fields();
        } else if config.has_pending_legacy_secret() {
            bail!(
                "use the legacy profile once to migrate its credential, or enter a replacement key"
            );
        }
        config.upsert_profile(profile).map_err(anyhow::Error::msg)?;
        config.active_profile_id = Some(provider_id.clone());
        crate::gateway_config::save(&write, &config).context("save provider profile")?;
        self.invalidate_computer_agent_authority();
        self.set_model(crate::gateway_config::model_selection_key(
            &provider_id,
            &model_id,
        ));
        let current_effort = self.inner.lock().effort;
        if current_effort != EffortLevel::None
            && !effort_options
                .iter()
                .any(|value| value == current_effort.as_str())
        {
            self.set_effort(EffortLevel::None);
        }
        Ok(())
    }

    pub async fn discover_provider_models(&self, provider_id: &str) -> Result<Vec<ModelInfo>> {
        // Authority is minted around the save inside, not held across the
        // network round-trip: a slow provider must not block the shutdown seal.
        self.ensure_accepting("discovering provider models")?;
        crate::provider_discovery::discover_profile_models(&self.write_authority(), provider_id)
            .await?;
        Ok(self
            .models()
            .into_iter()
            .filter(|model| model.provider_id == provider_id)
            .collect())
    }

    pub async fn qualify_provider_model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<crate::provider_qualification::ProviderQualificationReport> {
        // As above: mint at the write, not across the qualification probes.
        self.ensure_accepting("qualifying a provider model")?;
        crate::provider_qualification::qualify_provider_model(
            &self.write_authority(),
            provider_id,
            model_id,
        )
        .await
    }

    pub fn delete_provider_profile(&self, provider_id: &str) -> Result<()> {
        let write = self.durable_write("deleting a provider profile")?;
        let provider_id = crate::gateway_config::normalized_profile_id(provider_id)
            .map_err(anyhow::Error::msg)?;
        let mut config =
            crate::gateway_config::load_for_update().context("read provider profiles")?;
        if config
            .profile(&provider_id)
            .is_some_and(|profile| profile.managed_by_env)
        {
            bail!("environment-managed profiles are removed by unsetting their base URL variable");
        }
        let profile = config
            .remove_profile(&provider_id)
            .ok_or_else(|| anyhow!("unknown provider profile `{provider_id}`"))?;
        if config.has_pending_legacy_secret() {
            config.clear_legacy_fields();
        }
        crate::gateway_config::save(&write, &config).context("remove provider profile")?;
        self.invalidate_computer_agent_authority();

        if let Some(reference) = profile.credential_ref.as_deref() {
            crate::auth_store::delete_provider_credential(&profile.id, reference)
                .map_err(anyhow::Error::msg)?;
        }
        let selected_model = self.inner.lock().model.clone();
        let selected_provider = crate::gateway_config::parse_model_selection(&selected_model)
            .ok()
            .map(|selection| selection.provider_id);
        if selected_provider.as_deref() == Some(provider_id.as_str()) {
            self.set_model(crate::models_catalog::resolve_default_model());
        }
        Ok(())
    }

    pub fn set_plan_mode(&self, session_id: Uuid, enabled: bool) -> Result<()> {
        self.ensure_session_accepts_new_work(session_id)?;
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        s.plan_mode = enabled;
        if enabled && s.plan_status.is_empty() {
            s.plan_status = "proposed".into();
        }
        Ok(())
    }

    /// Accept the proposed plan and immediately start an execution turn that
    /// follows those steps (plan → execute pipeline).
    pub async fn accept_plan(&self, session_id: Uuid) -> Result<String> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (steps, goal) = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            if s.plan_steps.is_empty() {
                bail!("no plan to accept");
            }
            s.plan_mode = false;
            s.plan_status = "accepted".into();
            let steps = s.plan_steps.clone();
            let goal = s
                .plan_goal
                .clone()
                .unwrap_or_else(|| "complete the proposed plan".into());
            let tx = g.event_tx.clone();
            drop(g);
            let _ = tx.send(SessionUpdate::Plan {
                session_id,
                steps: steps.clone(),
                status: "accepted".into(),
            });
            (steps, goal)
        };

        let mut numbered = String::new();
        for (i, step) in steps.iter().enumerate() {
            numbered.push_str(&format!("{}. {}\n", i + 1, step));
        }
        let exec_prompt = format!(
            "Execute this accepted plan step by step using tools. \
             Do not re-plan unless blocked. When finished, summarize what you did.\n\n\
             Goal: {goal}\n\nPlan:\n{numbered}"
        );

        {
            let mut g = self.inner.lock();
            if let Some(s) = g.sessions.get_mut(&session_id) {
                s.plan_status = "executing".into();
            }
        }
        let reply = self.session_prompt(session_id, exec_prompt).await?;
        {
            let mut g = self.inner.lock();
            if let Some(s) = g.sessions.get_mut(&session_id) {
                s.plan_status = "done".into();
            }
            let tx = g.event_tx.clone();
            let steps = g
                .sessions
                .get(&session_id)
                .map(|s| s.plan_steps.clone())
                .unwrap_or_default();
            drop(g);
            let _ = tx.send(SessionUpdate::Plan {
                session_id,
                steps,
                status: "done".into(),
            });
        }
        Ok(reply)
    }

    pub fn reject_plan(&self, session_id: Uuid) -> Result<()> {
        self.ensure_session_accepts_new_work(session_id)?;
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        s.plan_mode = false;
        s.plan_steps.clear();
        s.plan_status = "rejected".into();
        s.plan_goal = None;
        let tx = g.event_tx.clone();
        drop(g);
        let _ = tx.send(SessionUpdate::Plan {
            session_id,
            steps: vec![],
            status: "rejected".into(),
        });
        Ok(())
    }

    fn current_turn_run_id(&self, session_id: Uuid) -> Option<String> {
        self.run_usage_trackers
            .lock()
            .get(&session_id)
            .map(|tracker| tracker.run_id().to_string())
    }

    fn insert_pending_permission(
        &self,
        session_id: Uuid,
        run_id: Option<String>,
        tool_name: impl Into<String>,
        summary: impl Into<String>,
        detail: serde_json::Value,
    ) -> (PermissionRequest, oneshot::Receiver<PermissionDecision>) {
        let req = PermissionRequest {
            id: Uuid::new_v4(),
            session_id,
            run_id: run_id.clone(),
            tool_name: tool_name.into(),
            summary: summary.into(),
            detail,
        };
        let (tx, rx) = oneshot::channel();
        {
            let mut g = self.inner.lock();
            g.pending_permissions.insert(
                req.id,
                PendingPermission {
                    tool_name: req.tool_name.clone(),
                    session_id,
                    run_id,
                    tx,
                },
            );
        }
        (req, rx)
    }

    async fn prompt_tool_permission(
        &self,
        session_id: Uuid,
        tool_name: &str,
        summary: String,
        detail: serde_json::Value,
        cancel: &CancellationToken,
    ) -> PermissionDecision {
        let run_id = self.current_turn_run_id(session_id);
        let (req, rx) =
            self.insert_pending_permission(session_id, run_id, tool_name, summary, detail);
        let _ = self.event_bus().send(SessionUpdate::PermissionRequired {
            session_id,
            request: req,
        });
        tokio::select! {
            decision = rx => decision.unwrap_or(PermissionDecision::Deny),
            _ = cancel.cancelled() => PermissionDecision::Deny,
        }
    }

    /// Non-consuming lookup of an in-memory permission oneshot.
    pub fn inspect_pending_permission(&self, request_id: Uuid) -> Option<PendingPermissionView> {
        let g = self.inner.lock();
        let pending = g.pending_permissions.get(&request_id)?;
        Some(PendingPermissionView {
            request_id,
            session_id: pending.session_id,
            run_id: pending.run_id.clone(),
            tool_name: pending.tool_name.clone(),
            receiver_open: !pending.tx.is_closed(),
        })
    }

    /// Insert a host pending permission bound to a Run, using the same path
    /// production tools use. The returned receiver is the in-memory oneshot.
    pub fn begin_pending_permission(
        &self,
        session_id: Uuid,
        run_id: Option<&str>,
        tool_name: &str,
        summary: &str,
    ) -> Result<(PermissionRequest, oneshot::Receiver<PermissionDecision>)> {
        {
            let g = self.inner.lock();
            if !g.sessions.contains_key(&session_id) {
                bail!("unknown session");
            }
        }
        let run_id = run_id
            .map(str::to_string)
            .or_else(|| self.current_turn_run_id(session_id));
        let (req, rx) = self.insert_pending_permission(
            session_id,
            run_id,
            tool_name,
            summary,
            serde_json::json!({ "tool": tool_name }),
        );
        let _ = self.event_bus().send(SessionUpdate::PermissionRequired {
            session_id,
            request: req.clone(),
        });
        Ok((req, rx))
    }

    pub fn permission_respond(&self, request_id: Uuid, decision: PermissionDecision) -> Result<()> {
        let mut g = self.inner.lock();
        let pending = g
            .pending_permissions
            .remove(&request_id)
            .ok_or_else(|| anyhow!("no pending permission {request_id}"))?;
        if pending.tx.is_closed() {
            return Err(anyhow!("permission receiver is gone"));
        }
        pending
            .tx
            .send(decision)
            .map_err(|_| anyhow!("permission receiver is gone"))?;
        // AlwaysAllow is per-tool only and must not persist if the oneshot
        // never received the decision. Global YOLO remains Settings/`set_always_approve`.
        if decision == PermissionDecision::AlwaysAllow && !pending.tool_name.is_empty() {
            g.always_allowed_tools.insert(pending.tool_name);
        }
        Ok(())
    }

    pub fn session_queue_list(&self, session_id: Uuid) -> Result<Vec<PromptQueueEntry>> {
        Ok(self.session_queue_snapshot(session_id)?.entries)
    }

    /// The queue plus the revision it was read at, taken under one lock.
    ///
    /// A refetch competes with the event stream, not just with other refetches:
    /// a list response can be overtaken by a newer `PromptQueueChanged` and
    /// then applied on top of it, silently restoring an older membership and
    /// ordering. Stamping the read with the newest committed revision lets a
    /// consumer put refetches and events through one ordering rule instead of
    /// two that cannot see each other.
    pub fn session_queue_snapshot(&self, session_id: Uuid) -> Result<PromptQueueSnapshot> {
        let g = self.inner.lock();
        if !g.sessions.contains_key(&session_id) {
            bail!("unknown session");
        }
        Ok(PromptQueueSnapshot {
            entries: g
                .prompt_queues
                .get(&session_id)
                .map(SessionPromptQueue::list)
                .unwrap_or_default(),
            revision: g.current_queue_revision(session_id),
        })
    }

    pub fn session_queue_add(
        &self,
        session_id: Uuid,
        text: String,
        priority: bool,
    ) -> Result<Vec<PromptQueueEntry>> {
        self.session_queue_add_with_source(
            session_id,
            text,
            priority,
            "composer",
            Some("desktop".into()),
        )
    }

    /// Queue with explicit source/owner metadata (MCP control uses `control` / `mcp`).
    pub fn session_queue_add_with_source(
        &self,
        session_id: Uuid,
        text: String,
        priority: bool,
        source: &str,
        owner: Option<String>,
    ) -> Result<Vec<PromptQueueEntry>> {
        self.session_queue_add_with_source_receipt(session_id, text, priority, source, owner)
            .map(|(entries, _, _)| entries)
    }

    /// Mutators that advance the queue revision return it, so a caller can
    /// continue — notably to a revision-fenced reorder — without a second read
    /// that could observe someone else's newer mutation.
    pub fn session_queue_add_with_source_receipt(
        &self,
        session_id: Uuid,
        text: String,
        priority: bool,
        source: &str,
        owner: Option<String>,
    ) -> Result<(Vec<PromptQueueEntry>, PromptQueueEntry, u64)> {
        let write = self.durable_write("queueing a prompt")?;
        self.ensure_session_accepts_new_work(session_id)?;
        let origin = owner.clone().unwrap_or_else(|| source.to_string());
        let (list, changed_entry, revision) = {
            let mut g = self.inner.lock();
            if !g.sessions.contains_key(&session_id) {
                bail!("unknown session");
            }
            let mut next = g
                .prompt_queues
                .get(&session_id)
                .cloned()
                .unwrap_or_default();
            let changed_entry = next.add_with_owner(text, source, priority, owner)?;
            session_store::save_prompt_queue(&write, session_id, &next)
                .map_err(|e| anyhow!("persist prompt queue: {e}"))?;
            let list = next.list();
            g.prompt_queues.insert(session_id, next);
            let revision = g.next_queue_revision(session_id);
            (list, changed_entry, revision)
        };
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            list.clone(),
            "queued",
            origin,
            Some(changed_entry.clone()),
            None,
        );
        Ok((list, changed_entry, revision))
    }

    pub fn session_queue_edit(
        &self,
        session_id: Uuid,
        entry_id: &str,
        version: u64,
        text: String,
    ) -> Result<Vec<PromptQueueEntry>> {
        self.session_queue_edit_with_origin(session_id, entry_id, version, text, "desktop")
            .map(|(entries, _)| entries)
    }

    pub fn session_queue_edit_with_origin(
        &self,
        session_id: Uuid,
        entry_id: &str,
        version: u64,
        text: String,
        origin: &str,
    ) -> Result<(Vec<PromptQueueEntry>, u64)> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (list, changed_entry, revision) = {
            let mut g = self.inner.lock();
            let queue = g
                .prompt_queues
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("no prompt queue for session {session_id}"))?;
            let changed_entry = queue.edit(entry_id, version, text)?;
            let list = queue.list();
            let revision = g.next_queue_revision(session_id);
            (list, changed_entry, revision)
        };
        let _ = self.persist_prompt_queue(session_id);
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            list.clone(),
            "edited",
            origin.to_string(),
            Some(changed_entry),
            None,
        );
        Ok((list, revision))
    }

    pub fn session_queue_remove(
        &self,
        session_id: Uuid,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<Vec<PromptQueueEntry>> {
        self.session_queue_remove_with_origin(session_id, entry_id, "desktop", expected_version)
    }

    pub fn session_queue_remove_with_origin(
        &self,
        session_id: Uuid,
        entry_id: &str,
        origin: &str,
        expected_version: u64,
    ) -> Result<Vec<PromptQueueEntry>> {
        self.session_queue_remove_with_origin_receipt(
            session_id,
            entry_id,
            origin,
            expected_version,
        )
        .map(|(entries, _, _)| entries)
    }

    pub fn session_queue_remove_with_origin_receipt(
        &self,
        session_id: Uuid,
        entry_id: &str,
        origin: &str,
        expected_version: u64,
    ) -> Result<(Vec<PromptQueueEntry>, PromptQueueEntry, u64)> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (list, changed_entry, revision) = {
            let mut g = self.inner.lock();
            let queue = g
                .prompt_queues
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("no prompt queue for session {session_id}"))?;
            queue.check_version(entry_id, expected_version)?;
            let changed_entry = queue.remove(entry_id)?;
            let list = queue.list();
            let revision = g.next_queue_revision(session_id);
            (list, changed_entry, revision)
        };
        let _ = self.persist_prompt_queue(session_id);
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            list.clone(),
            "removed",
            origin.to_string(),
            Some(changed_entry.clone()),
            None,
        );
        Ok((list, changed_entry, revision))
    }

    pub fn session_queue_clear(&self, session_id: Uuid) -> Result<Vec<PromptQueueEntry>> {
        self.session_queue_clear_with_origin(session_id, "desktop")
    }

    pub fn session_queue_clear_with_origin(
        &self,
        session_id: Uuid,
        origin: &str,
    ) -> Result<Vec<PromptQueueEntry>> {
        self.session_queue_clear_with_origin_receipt(session_id, origin)
            .map(|(entries, _, _)| entries)
    }

    /// Clear plus the outcome describing what could not be stopped.
    ///
    /// Callers that hand a receipt to a coordinator must use this variant:
    /// an empty `entries` list alone does not mean the session is quiet,
    /// because steering already delivered to a model boundary is
    /// unretractable (see [`PromptQueueClearOutcome`]).
    pub fn session_queue_clear_with_origin_receipt(
        &self,
        session_id: Uuid,
        origin: &str,
    ) -> Result<(Vec<PromptQueueEntry>, PromptQueueClearOutcome, u64)> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (outcome, revision) = {
            let mut g = self.inner.lock();
            if !g.sessions.contains_key(&session_id) {
                bail!("unknown session");
            }
            let outcome = g.prompt_queues.entry(session_id).or_default().clear();
            let revision = g.next_queue_revision(session_id);
            (outcome, revision)
        };
        let _ = self.persist_prompt_queue(session_id);
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            Vec::new(),
            "cleared",
            origin.to_string(),
            None,
            None,
        );
        Ok((Vec::new(), outcome, revision))
    }

    pub fn session_queue_move(
        &self,
        session_id: Uuid,
        entry_id: &str,
        to_index: usize,
        expected_version: u64,
        expected_revision: u64,
    ) -> Result<(Vec<PromptQueueEntry>, u64)> {
        self.session_queue_move_with_origin(
            session_id,
            entry_id,
            to_index,
            "desktop",
            expected_version,
            expected_revision,
        )
    }

    /// The desktop reorders under the same revision fence as the control plane.
    ///
    /// `to_index` is absolute, so it only means something against a specific
    /// ordering, and the per-entry CAS cannot see a `run_next` that displaced
    /// entries without changing their versions. Exempting the desktop would
    /// leave that hole open from the other writer — the same reason S3 made
    /// `expected_version` mandatory here rather than MCP-only.
    pub fn session_queue_move_with_origin(
        &self,
        session_id: Uuid,
        entry_id: &str,
        to_index: usize,
        origin: &str,
        expected_version: u64,
        expected_revision: u64,
    ) -> Result<(Vec<PromptQueueEntry>, u64)> {
        self.session_queue_move_with_origin_impl(
            session_id,
            entry_id,
            to_index,
            origin,
            expected_version,
            Some(expected_revision),
        )
    }

    /// Reorder an entry with both its per-entry CAS and the queue revision
    /// that gives an absolute `to_index` meaning.
    pub fn session_queue_move_with_origin_and_revision(
        &self,
        session_id: Uuid,
        entry_id: &str,
        to_index: usize,
        origin: &str,
        expected_version: u64,
        expected_revision: u64,
    ) -> Result<(Vec<PromptQueueEntry>, u64)> {
        self.session_queue_move_with_origin_impl(
            session_id,
            entry_id,
            to_index,
            origin,
            expected_version,
            Some(expected_revision),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn session_queue_move_with_origin_impl(
        &self,
        session_id: Uuid,
        entry_id: &str,
        to_index: usize,
        origin: &str,
        expected_version: u64,
        expected_revision: Option<u64>,
    ) -> Result<(Vec<PromptQueueEntry>, u64)> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (list, changed_entry, revision) = {
            let mut g = self.inner.lock();
            let current_revision = g
                .prompt_queue_revisions
                .get(&session_id)
                .copied()
                .unwrap_or_default();
            if let Some(expected_revision) = expected_revision {
                if current_revision != expected_revision {
                    bail!(
                        "stale prompt queue revision: expected {expected_revision}, current {current_revision}"
                    );
                }
            }
            let queue = g
                .prompt_queues
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("no prompt queue for session {session_id}"))?;
            queue.check_version(entry_id, expected_version)?;
            queue.move_to(entry_id, to_index)?;
            let list = queue.list();
            // Post-move, because reordering now bumps the versions of every
            // entry that shifted: a pre-move copy would hand the caller a
            // version its own next CAS would be rejected for.
            let changed_entry = list.iter().find(|entry| entry.id == entry_id).cloned();
            let revision = g.next_queue_revision(session_id);
            (list, changed_entry, revision)
        };
        let _ = self.persist_prompt_queue(session_id);
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            list.clone(),
            "reordered",
            origin.to_string(),
            changed_entry.clone(),
            None,
        );
        Ok((list, revision))
    }

    /// Drain the next batch and claim the session's turn slot for it.
    ///
    /// Draining and starting the turn are separate calls, so without a
    /// reservation another writer can start a turn in the gap: the start is
    /// then refused and the batch is already gone from the queue, which loses
    /// the prompt outright. Taking the batch and reserving the turn under one
    /// lock makes the handoff atomic. The caller must present
    /// `result.reservation` when starting the turn, or call
    /// [`Self::session_queue_restore_drain`] to give both back.
    pub fn session_queue_take_next(&self, session_id: Uuid) -> Result<PromptQueueTakeResult> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (result, revision) = {
            let mut g = self.inner.lock();
            if !g.sessions.contains_key(&session_id) {
                bail!("unknown session");
            }
            // A reservation counts as busy too: draining into a session an
            // orchestration run has already claimed would hit the same refusal.
            // An abandoned drain reservation is reclaimed first, so a drainer
            // that died between taking a batch and starting its turn cannot
            // leave the session busy forever.
            g.reclaim_expired_drain_reservation(session_id);
            let busy = g.turn_cancels.contains_key(&session_id)
                || g.turn_reservations.contains_key(&session_id);
            let queue = g.prompt_queues.entry(session_id).or_default();
            if busy {
                let entries = queue.list();
                (
                    PromptQueueTakeResult {
                        batch: None,
                        entries,
                        reservation: None,
                    },
                    None,
                )
            } else {
                let mut result = queue.take_next();
                // Only a real drain mutates the queue, so only that stamps
                // a revision and only that claims the turn slot.
                let revision = if result.batch.is_some() {
                    let owner = format!("queue-drain:{}", Uuid::new_v4());
                    g.turn_reservations.insert(session_id, owner.clone());
                    g.drain_reservations
                        .insert(session_id, std::time::Instant::now());
                    result.reservation = Some(owner);
                    Some(g.next_queue_revision(session_id))
                } else {
                    None
                };
                (result, revision)
            }
        };
        if let (Some(batch), Some(revision)) = (result.batch.as_ref(), revision) {
            let _ = self.persist_prompt_queue(session_id);
            self.emit_prompt_queue_changed(
                session_id,
                revision,
                result.entries.clone(),
                "delivered",
                "desktop".into(),
                batch.entries.first().cloned(),
                None,
            );
        }
        Ok(result)
    }

    /// Hand a drained batch back after its turn failed to start.
    ///
    /// Releases the reservation taken by [`Self::session_queue_take_next`] and
    /// pushes the entries back at the head in their original order, so a drain
    /// whose turn never began is a no-op rather than a lost prompt. Safe to
    /// call with a reservation that is no longer held — the entries are still
    /// restored, because the batch being out of the queue is the part that
    /// loses data.
    pub fn session_queue_restore_drain(
        &self,
        session_id: Uuid,
        reservation: Option<&str>,
        entries: Vec<PromptQueueEntry>,
    ) -> Result<Vec<PromptQueueEntry>> {
        if entries.is_empty() {
            if let Some(owner) = reservation {
                self.release_turn_reservation(session_id, owner);
            }
            return self.session_queue_list(session_id);
        }
        let (list, revision) = {
            let mut g = self.inner.lock();
            if let Some(owner) = reservation {
                if g.turn_reservations.get(&session_id).map(String::as_str) == Some(owner) {
                    g.turn_reservations.remove(&session_id);
                    g.drain_reservations.remove(&session_id);
                }
            }
            let queue = g.prompt_queues.entry(session_id).or_default();
            queue.restore_batch(entries);
            let list = queue.list();
            let revision = g.next_queue_revision(session_id);
            (list, revision)
        };
        self.orchestration_wakeup.notify_waiters();
        let _ = self.persist_prompt_queue(session_id);
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            list.clone(),
            "restored",
            "desktop".to_string(),
            list.first().cloned(),
            None,
        );
        Ok(list)
    }

    pub fn session_queue_run_next(
        &self,
        session_id: Uuid,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<PromptQueueRunNextResult> {
        self.session_queue_run_next_with_origin(session_id, entry_id, "desktop", expected_version)
            .map(|(result, _)| result)
    }

    /// Promote an entry to the head and cancel the active turn so it runs next.
    ///
    /// The cancel happens only after the CAS and the promotion have both
    /// succeeded: a stale `expected_version` returns before the lock is
    /// released, so a losing coordinator never interrupts a running turn.
    pub fn session_queue_run_next_with_origin(
        &self,
        session_id: Uuid,
        entry_id: &str,
        origin: &str,
        expected_version: u64,
    ) -> Result<(PromptQueueRunNextResult, u64)> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (changed_entry, active_generation, revision) = {
            let mut g = self.inner.lock();
            let queue = g
                .prompt_queues
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("no prompt queue for session {session_id}"))?;
            queue.check_version(entry_id, expected_version)?;
            let changed_entry = queue.run_next(entry_id)?;
            // Capture *which* turn is active, not merely that one is. The
            // observed turn can finish before the cancel below, and an
            // unconditional cancel would then interrupt whichever turn started
            // next — a turn this caller never observed.
            let active_generation = g.turn_generations.get(&session_id).copied();
            let revision = g.next_queue_revision(session_id);
            (changed_entry, active_generation, revision)
        };
        let _ = self.persist_prompt_queue(session_id);
        let cancelled_active = active_generation.is_some_and(|generation| {
            self.cancel_turn_if_generation(session_id, generation)
                .is_ok()
        });
        let result = PromptQueueRunNextResult {
            entries: self.session_queue_list(session_id)?,
            cancelled_active,
            changed_entry: changed_entry.clone(),
        };
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            result.entries.clone(),
            "run_next",
            origin.to_string(),
            Some(changed_entry),
            None,
        );
        Ok((result, revision))
    }

    pub fn session_queue_steer_entry(
        &self,
        session_id: Uuid,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<SteeringReceipt> {
        self.session_queue_steer_entry_with_origin(
            session_id,
            entry_id,
            "desktop",
            expected_version,
        )
        .map(|(receipt, _)| receipt)
    }

    pub fn session_queue_steer_entry_with_origin(
        &self,
        session_id: Uuid,
        entry_id: &str,
        origin: &str,
        expected_version: u64,
    ) -> Result<(SteeringReceipt, u64)> {
        self.ensure_session_accepts_new_work(session_id)?;
        let (receipt, revision) = {
            let mut g = self.inner.lock();
            let is_build = g
                .sessions
                .get(&session_id)
                .map(|session| session.kind == SessionKind::Build)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let can_inject = is_build && g.turn_cancels.contains_key(&session_id);
            let queue = g
                .prompt_queues
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("no prompt queue for session {session_id}"))?;
            queue.check_version(entry_id, expected_version)?;
            let receipt = queue.steer_queued(entry_id, can_inject)?;
            let revision = g.next_queue_revision(session_id);
            (receipt, revision)
        };
        let _ = self.persist_prompt_queue(session_id);
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            receipt.entries.clone(),
            "steer_now",
            origin.to_string(),
            Some(receipt.entry.clone()),
            Some(receipt.disposition),
        );
        Ok((receipt, revision))
    }

    pub fn session_steer(&self, session_id: Uuid, text: String) -> Result<SteeringReceipt> {
        self.session_steer_with_owner(session_id, text, Some("desktop".into()))
            .map(|(receipt, _)| receipt)
    }

    pub fn session_steer_with_owner(
        &self,
        session_id: Uuid,
        text: String,
        owner: Option<String>,
    ) -> Result<(SteeringReceipt, u64)> {
        let write = self.durable_write("steering a session")?;
        self.ensure_session_accepts_new_work(session_id)?;
        let origin = owner.clone().unwrap_or_else(|| "desktop".into());
        let (receipt, revision) = {
            let mut g = self.inner.lock();
            let is_build = g
                .sessions
                .get(&session_id)
                .map(|session| session.kind == SessionKind::Build)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let can_inject = is_build && g.turn_cancels.contains_key(&session_id);
            let mut next = g
                .prompt_queues
                .get(&session_id)
                .cloned()
                .unwrap_or_default();
            let receipt = next.steer_text_with_owner(text, can_inject, owner)?;
            session_store::save_prompt_queue(&write, session_id, &next)
                .map_err(|e| anyhow!("persist prompt queue: {e}"))?;
            g.prompt_queues.insert(session_id, next);
            let revision = g.next_queue_revision(session_id);
            (receipt, revision)
        };
        self.emit_prompt_queue_changed(
            session_id,
            revision,
            receipt.entries.clone(),
            "steer_now",
            origin,
            Some(receipt.entry.clone()),
            Some(receipt.disposition),
        );
        Ok((receipt, revision))
    }

    /// Cancel the in-flight turn for `session_id`, or every active turn when
    /// `session_id` is `None` (shutdown / global stop).
    /// Cancel a session's turn **only if** it is still the turn identified by
    /// `generation`.
    ///
    /// `run_next` observes the active turn while holding the queue lock but
    /// cannot cancel under it, because teardown re-enters the lock. Between
    /// those two points the observed turn can finish and a new one can start,
    /// and an unconditional `cancel_turn` would then kill the newcomer — a
    /// turn the caller never saw and never asked to interrupt. Re-checking the
    /// identity closes that window: a mismatch means the observed turn is
    /// already gone, so there is nothing this call is entitled to cancel.
    fn cancel_turn_if_generation(&self, session_id: Uuid, generation: u64) -> Result<()> {
        self.cancel_turn_checked(Some(session_id), Some(generation))
    }

    /// Test helper: back-date a drain reservation so the reclaim path is
    /// reachable without sleeping for the TTL.
    pub fn expire_drain_reservation_for_test(&self, session_id: Uuid) {
        let mut g = self.inner.lock();
        if let Some(taken) = g.drain_reservations.get_mut(&session_id) {
            *taken = std::time::Instant::now() - DRAIN_RESERVATION_TTL;
        }
    }

    /// Test helper: cancel only if `generation` is still the live turn.
    pub fn cancel_turn_if_generation_for_test(
        &self,
        session_id: Uuid,
        generation: u64,
    ) -> Result<()> {
        self.cancel_turn_if_generation(session_id, generation)
    }

    pub fn cancel_turn(&self, session_id: Option<Uuid>) -> Result<()> {
        self.cancel_turn_checked(session_id, None)
    }

    fn cancel_turn_checked(
        &self,
        session_id: Option<Uuid>,
        expect_generation: Option<u64>,
    ) -> Result<()> {
        // The identity check lives inside `cancel_turn_prepare`, under the one
        // lock acquisition that also fires the token: checking first and
        // cancelling afterwards would leave exactly the window it is meant to
        // close.
        let (live_shells, kill_ids) = self.cancel_turn_prepare(session_id, expect_generation)?;
        // Fire-and-forget kill for sync callers (desktop stop button).
        let handle = tokio::runtime::Handle::try_current();
        if let Ok(h) = handle {
            h.spawn(async move {
                kill_shells(live_shells, kill_ids).await;
            });
        } else if let Ok(mut map) = live_shells.try_lock() {
            for id in kill_ids {
                if let Some(mut child) = map.remove(&id) {
                    crate::process_tree::terminate_now(&mut child);
                }
            }
        }
        Ok(())
    }

    /// Cancel turn and **await** shell/subagent teardown (duration limits / orchestration).
    pub async fn cancel_turn_and_await(&self, session_id: Option<Uuid>) -> Result<()> {
        let (live_shells, kill_ids) = self.cancel_turn_prepare(session_id, None)?;
        kill_shells(live_shells, kill_ids).await;
        // Brief settle so cancel tokens propagate.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        Ok(())
    }

    fn cancel_turn_prepare(
        &self,
        session_id: Option<Uuid>,
        expect_generation: Option<u64>,
    ) -> Result<(local_tools::LiveShellMap, Vec<Uuid>)> {
        let mut g = self.inner.lock();
        match session_id {
            Some(id) => {
                if let Some(expected) = expect_generation {
                    if g.turn_generations.get(&id).copied() != Some(expected) {
                        bail!("turn {expected} for session {id} is no longer active");
                    }
                }
                let Some(c) = g.turn_cancels.get(&id) else {
                    bail!("no active turn for session {id}");
                };
                c.cancel();
                let sid = id.to_string();
                let child_ids: Vec<String> = g
                    .subagents
                    .iter()
                    .filter(|s| {
                        s.session_id.as_deref() == Some(sid.as_str()) && s.status == "running"
                    })
                    .map(|s| s.id.clone())
                    .collect();
                for cid in &child_ids {
                    if let Some(tok) = g.subagent_cancels.remove(cid) {
                        tok.cancel();
                    }
                    if let Some(s) = g.subagents.iter_mut().find(|s| s.id == *cid) {
                        s.status = "cancelled".into();
                        s.summary = Some("parent turn cancelled".into());
                    }
                }
                Ok((g.live_shells.clone(), vec![id]))
            }
            None => {
                if g.turn_cancels.is_empty() {
                    bail!("no active turn");
                }
                for c in g.turn_cancels.values() {
                    c.cancel();
                }
                for tok in g.subagent_cancels.values() {
                    tok.cancel();
                }
                g.subagent_cancels.clear();
                for s in g.subagents.iter_mut() {
                    if s.status == "running" {
                        s.status = "cancelled".into();
                        s.summary = Some("parent turn cancelled".into());
                    }
                }
                let ids: Vec<Uuid> = g.turn_cancels.keys().copied().collect();
                Ok((g.live_shells.clone(), ids))
            }
        }
    }

    /// Run a turn. Returns the final assistant text so the UI always has a
    /// reply even if event delivery is delayed.
    ///
    /// Multiple sessions may run turns concurrently; each keeps its own
    /// cancellation token keyed by `session_id`.
    pub async fn session_prompt(&self, session_id: Uuid, prompt: String) -> Result<String> {
        self.session_prompt_with_max_rounds(session_id, prompt, None)
            .await
    }

    /// Like [`session_prompt`] but applies a per-turn model-round budget
    /// (orchestration `RunBounds.max_rounds`). `None` uses host default.
    pub async fn session_prompt_with_max_rounds(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
    ) -> Result<String> {
        self.session_prompt_inner(session_id, prompt, max_rounds, None, None, None)
            .await
    }

    /// Start a turn using a reservation previously created by `reserve_turn`.
    pub async fn session_prompt_reserved_with_max_rounds(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
        owner: &str,
    ) -> Result<String> {
        self.session_prompt_inner(session_id, prompt, max_rounds, Some(owner), None, None)
            .await
    }

    /// Start a reserved turn under a durable run identity owned by an
    /// external coordinator. The host must not create a second desktop run
    /// for this turn; the coordinator's record remains the source of truth.
    pub async fn session_prompt_reserved_with_max_rounds_for_run(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
        owner: &str,
        run_id: &str,
        execution_mode: RunExecutionMode,
    ) -> Result<String> {
        self.session_prompt_inner(
            session_id,
            prompt,
            max_rounds,
            Some(owner),
            Some(ExternalRunContext {
                run_id: run_id.to_string(),
                execution_mode,
            }),
            None,
        )
        .await
    }

    /// One prompt turn, registered on the shutdown join barrier **before** any
    /// of it starts.
    ///
    /// A turn is the crate's largest external effect: it sends to the provider
    /// over the network and runs tools that edit the user's workspace and spawn
    /// child processes. It was supervised only when a caller happened to spawn
    /// it inside a supervised task — the orchestration service does, but a
    /// desktop Tauri command and a direct embedder call do not. Registration
    /// therefore belongs at the effect, not at each call site: put here, the
    /// turn is on the barrier before its first poll, so there is no window in
    /// which it has started but shutdown cannot see it (#455).
    async fn session_prompt_inner(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
        reservation_owner: Option<&str>,
        external_run: Option<ExternalRunContext>,
        resume: Option<AgentContinuationPlan>,
    ) -> Result<String> {
        let effect = self.session_prompt_effect(
            session_id,
            prompt,
            max_rounds,
            reservation_owner,
            external_run,
            resume,
        );
        // `track_supervised` registers before returning, so the count rises
        // here rather than when the future is first polled.
        self.track_supervised("running a prompt turn", effect)?
            .await
    }

    async fn session_prompt_effect(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
        reservation_owner: Option<&str>,
        external_run: Option<ExternalRunContext>,
        resume: Option<AgentContinuationPlan>,
    ) -> Result<String> {
        self.ensure_session_accepts_new_work(session_id)?;
        self.ensure_transcript_loaded(session_id)?;
        self.ensure_build_workspace_ready(session_id)?;
        let persistent_agent = {
            let kind = self
                .inner
                .lock()
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?
                .kind;
            if kind == SessionKind::Build {
                Some(self.ensure_session_agent(session_id)?)
            } else {
                None
            }
        };
        let external_agent_spec = if let Some(external) = external_run.as_ref() {
            let store = self.ensure_orchestration_store()?;
            let run = store
                .load_run(&external.run_id)?
                .ok_or_else(|| anyhow!("external Run disappeared before turn start"))?;
            let agent = persistent_agent
                .as_ref()
                .ok_or_else(|| anyhow!("external Build Run has no persistent Agent"))?;
            if run.state != RunState::Running
                || run.session_id != session_id
                || run.agent_id.as_deref() != Some(agent.agent_id.as_str())
                || agent.current_run_id.as_deref() != Some(run.run_id.as_str())
            {
                bail!("external Run activation does not match the persistent Agent");
            }
            let revision = run
                .agent_spec_revision
                .ok_or_else(|| anyhow!("external Run has no captured Agent specification"))?;
            if agent.current_spec()?.revision != revision {
                bail!("persistent Agent specification changed before external turn start");
            }
            Some(
                store
                    .load_agent_spec(&agent.agent_id, revision)?
                    .ok_or_else(|| anyhow!("external Run Agent specification is missing"))?,
            )
        } else {
            None
        };
        let agent_default_bounds = if let Some(spec) = external_agent_spec.as_ref() {
            Some(spec.default_run_bounds.clone())
        } else {
            persistent_agent
                .as_ref()
                .map(|agent| {
                    agent
                        .current_spec()
                        .map(|spec| spec.default_run_bounds.clone())
                        .map_err(|error| anyhow!(error.to_string()))
                })
                .transpose()?
        };
        let effective_agent_bounds = resume
            .as_ref()
            .map(|plan| plan.effective_run_bounds.clone())
            .or(agent_default_bounds.clone());
        if let Some(bounds) = effective_agent_bounds.as_ref() {
            let continuation_bytes = resume
                .as_ref()
                .map(|plan| plan.context.prompt_bytes)
                .unwrap_or_default();
            if prompt.len().saturating_add(continuation_bytes) > bounds.max_prompt_bytes {
                bail!(
                    "prompt plus continuation context exceeds persistent Agent max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                );
            }
        }
        let effective_max_rounds = if let Some(bounds) = effective_agent_bounds.as_ref() {
            let default = bounds.max_rounds;
            let ambient = self.inner.lock().max_agent_rounds.unwrap_or(default);
            Some(
                max_rounds
                    .unwrap_or(default)
                    .min(default)
                    .min(ambient)
                    .max(1),
            )
        } else {
            max_rounds
        };
        if let Some(plan) = resume.as_ref() {
            let (workspace, agent_id) = {
                let g = self.inner.lock();
                let session = g
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("unknown session"))?;
                (session.cwd.display().to_string(), session.agent_id.clone())
            };
            if agent_id.as_deref() != Some(plan.agent.agent_id.as_str()) {
                bail!("resume agent does not match the session binding");
            }
            AgentResumePlan {
                agent: plan.agent.clone(),
                checkpoint: plan.checkpoint.clone(),
                parent_run_id: plan.parent_run_id.clone(),
            }
            .validate_for(session_id, &workspace)
            .map_err(|error| anyhow!(error.to_string()))?;
            let store = self.ensure_orchestration_store()?;
            let current_agent = store
                .load_agent(&plan.agent.agent_id)?
                .ok_or_else(|| anyhow!("persistent Agent disappeared after preparation"))?;
            if current_agent.latest_checkpoint_id.as_deref()
                != Some(plan.checkpoint.checkpoint_id.as_str())
                || current_agent.current_spec()?.revision
                    != plan.input_snapshot.execution_spec.revision
            {
                bail!("persistent Agent specification or checkpoint changed after preparation");
            }
            if store
                .load_continuation_input(&plan.input_snapshot.input_hash)?
                .as_ref()
                != Some(&plan.input_snapshot)
                || store
                    .load_continuation_context(&plan.context.context_id)?
                    .as_ref()
                    != Some(&plan.context)
            {
                bail!("prepared continuation context is missing or does not match durable input");
            }
        }
        let resume_context = resume
            .as_ref()
            .map(|plan| plan.context.rendered_context.clone());
        let defer_resume_transcript = resume_context.is_some();
        let (cwd, model, effort, plan_mode, kind, execution_mode, cancel, event_tx) = {
            let mut g = self.inner.lock();
            if !g.running {
                bail!("agent not started");
            }
            // One in-flight turn per session (re-prompt while busy is an error).
            if g.turn_cancels.contains_key(&session_id) {
                bail!("session already has an active turn");
            }
            match reservation_owner {
                Some(owner)
                    if g.turn_reservations.get(&session_id).map(String::as_str) == Some(owner) =>
                {
                    g.turn_reservations.remove(&session_id);
                    g.drain_reservations.remove(&session_id);
                }
                Some(_) => bail!("missing or mismatched turn reservation"),
                None if g.turn_reservations.contains_key(&session_id) => {
                    bail!("session already has an active turn");
                }
                None => {}
            }
            // Persistent Agent model selection is revisioned and must not
            // drift with the currently focused desktop model.
            let model = if let Some(spec) = external_agent_spec.as_ref() {
                spec.model.selection_key.clone()
            } else {
                persistent_agent
                    .as_ref()
                    .map(|agent| {
                        agent
                            .current_spec()
                            .map(|spec| spec.model.selection_key.clone())
                            .map_err(|error| anyhow!(error.to_string()))
                    })
                    .transpose()?
                    .unwrap_or_else(|| g.model.clone())
            };
            let effort = g.effort;
            let cancel = CancellationToken::new();
            g.turn_cancels.insert(session_id, cancel.clone());
            g.begin_turn_generation(session_id);
            if let Some(n) = effective_max_rounds {
                g.turn_max_rounds.insert(session_id, n.max(1));
            } else {
                g.turn_max_rounds.remove(&session_id);
            }
            g.active_session = Some(session_id);
            let event_tx = g.event_tx.clone();
            let s = g
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.model = model.clone();
            s.effort = effort;
            if !defer_resume_transcript {
                s.transcript.push(TranscriptEntry::user(prompt.clone()));
                if s.title == "New session" || s.title == "New chat" {
                    s.title = prompt.chars().take(48).collect();
                }
            }
            s.updated_at = Utc::now();
            (
                s.cwd.clone(),
                model,
                effort,
                s.plan_mode,
                s.kind,
                s.execution_mode,
                cancel,
                event_tx,
            )
        };
        // RAII immediately after insert — before any fallible work — so a panic
        // in persist_session cannot leave the session permanently busy.
        let mut busy_guard = TurnBusyGuard {
            host: self.clone(),
            session_id,
            armed: true,
        };
        // Durably append the user turn before the long model call.
        self.persist_session(session_id);
        let start_seq = event_tx.current_seq();
        let usage_before = self.session_usage_snapshot(session_id);
        let turn_id = Uuid::new_v4();
        let agent = persistent_agent;
        let requested_execution_mode = external_run
            .as_ref()
            .map(|run| run.execution_mode)
            .unwrap_or(execution_mode);
        if external_run.is_some()
            && requested_execution_mode != RunExecutionMode::Shared
            && kind != SessionKind::Build
        {
            bail!("isolated external execution is available only for Build sessions");
        }
        let run_execution = if kind == SessionKind::Build
            && requested_execution_mode == RunExecutionMode::IsolatedWorktree
        {
            let run_id = external_run
                .as_ref()
                .map(|run| run.run_id.clone())
                .unwrap_or_else(|| format!("desktop-{turn_id}"));
            let prepared = run_promotion::prepare(&cwd, &run_id)?;
            let execution = RunExecution {
                mode: RunExecutionMode::IsolatedWorktree,
                source_workspace: cwd.display().to_string(),
                execution_workspace: prepared.cwd.display().to_string(),
                base_revision: prepared.base_revision,
                source_fingerprint: prepared.source_fingerprint,
                final_fingerprint: None,
                promotion_state: PromotionState::Preparing,
                promoted_at: None,
            };
            if let Some(external) = external_run.as_ref() {
                let store = match self.ensure_orchestration_store() {
                    Ok(store) => store,
                    Err(error) => {
                        let _ =
                            run_promotion::discard(&cwd, Path::new(&execution.execution_workspace));
                        return Err(error);
                    }
                };
                let updated = match store.update_run(&external.run_id, |run| {
                    if run.session_id != session_id {
                        bail!("external run session does not match turn session");
                    }
                    run.execution = Some(execution.clone());
                    run.updated_at = Utc::now();
                    Ok(())
                }) {
                    Ok(updated) => updated,
                    Err(error) => {
                        let _ =
                            run_promotion::discard(&cwd, Path::new(&execution.execution_workspace));
                        return Err(error);
                    }
                };
                if updated.is_none() {
                    let _ = run_promotion::discard(&cwd, Path::new(&execution.execution_workspace));
                    bail!("external run disappeared before execution could be attached");
                }
            }
            Some(execution)
        } else {
            None
        };
        let execution_cwd = run_execution
            .as_ref()
            .map(|execution| PathBuf::from(&execution.execution_workspace))
            .unwrap_or_else(|| cwd.clone());
        let desktop_run = if external_run.is_none() && kind == SessionKind::Build {
            let mut bounds = effective_agent_bounds.clone().unwrap_or_default();
            if let Some(rounds) = effective_max_rounds {
                bounds.max_rounds = bounds.max_rounds.min(rounds).max(1);
            }
            self.begin_desktop_run(
                session_id,
                &cwd,
                &prompt,
                bounds,
                start_seq,
                turn_id,
                run_execution.clone(),
                agent.as_ref().map(|agent| agent.agent_id.clone()),
                agent
                    .as_ref()
                    .and_then(|agent| agent.current_spec().ok().map(|spec| spec.revision)),
                resume.as_ref().map(|plan| plan.parent_run_id.clone()),
                resume.as_ref(),
            )
        } else {
            None
        };
        if resume.is_some() && desktop_run.is_none() {
            bail!("persistent continuation could not create and activate its durable Run");
        }
        if let Some(context) = resume_context.as_deref() {
            let mut g = self.inner.lock();
            let session = g
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("resume Lane disappeared after durable admission"))?;
            session.transcript.push(TranscriptEntry::system(context));
            session
                .transcript
                .push(TranscriptEntry::user(prompt.clone()));
            if session.title == "New session" || session.title == "New chat" {
                session.title = prompt.chars().take(48).collect();
            }
            session.updated_at = Utc::now();
            drop(g);
            self.persist_session(session_id);
        }
        let run_usage_tracker = if let Some(external) = external_run.as_ref() {
            let store = self.ensure_orchestration_store()?;
            let run = store
                .load_run(&external.run_id)?
                .ok_or_else(|| anyhow!("external run disappeared before token accounting"))?;
            Some(RunUsageTracker::from_run(store, &run))
        } else if let Some((run_id, store)) = desktop_run.as_ref() {
            store
                .load_run(run_id)?
                .map(|run| RunUsageTracker::from_run(store.clone(), &run))
        } else {
            None
        };
        if let Some(tracker) = run_usage_tracker.as_ref() {
            self.run_usage_trackers
                .lock()
                .insert(session_id, tracker.clone());
        }
        let mut desktop_aggregator = desktop_run.as_ref().and_then(|(run_id, store)| {
            self.start_desktop_run_aggregator(run_id, session_id, store.clone())
        });
        let _ = event_tx.send(SessionUpdate::TurnStarted {
            session_id,
            turn_id,
        });

        let run_turn = self.run_turn(
            session_id,
            &execution_cwd,
            &model,
            effort,
            plan_mode,
            kind,
            &prompt,
            cancel.clone(),
            event_tx.clone(),
        );
        tokio::pin!(run_turn);
        let mut duration_limited = false;
        let mut result = if let Some(duration_ms) = desktop_run
            .as_ref()
            .and_then(|(run_id, store)| store.load_run(run_id).ok().flatten())
            .map(|run| run.bounds.max_duration_ms)
        {
            tokio::select! {
                result = &mut run_turn => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(duration_ms.max(1))) => {
                    duration_limited = true;
                    cancel.cancel();
                    let message = format!(
                        "Stopped at the persistent Agent duration limit of {duration_ms} ms"
                    );
                    emit_message(&event_tx, session_id, &message);
                    Err(anyhow!(message))
                }
            }
        } else {
            run_turn.await
        };
        if let Some(tracker) = run_usage_tracker.as_ref() {
            if let Err(error) = self
                .quiesce_bounded_run_subagents(session_id, tracker)
                .await
            {
                result = Err(error);
            }
            if let (Ok(text), Some(stop)) = (&mut result, tracker.stop_message()) {
                if !text.contains("Stopped at the run token boundary") {
                    if !text.trim().is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&stop);
                    self.replace_last_assistant_text(session_id, text);
                    self.persist_session_rewrite(session_id);
                    emit_message(&event_tx, session_id, &stop);
                }
            }
        }
        let durable_stop_code = run_usage_tracker
            .as_ref()
            .and_then(|tracker| tracker.durable_stop_code());
        if let Some(expected) = run_usage_tracker.as_ref() {
            let mut trackers = self.run_usage_trackers.lock();
            if trackers
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(current, expected))
            {
                trackers.remove(&session_id);
            }
        }

        // Append assistant turn(s) written by push_assistant.
        self.persist_session(session_id);
        self.persist_chrome();

        let cancelled = cancel.is_cancelled();
        let end_seq = event_tx.current_seq();
        let (observations, mut changed_paths, tests_passed) =
            match event_tx.read_range_all(start_seq, Some(end_seq), Some(session_id)) {
                Ok(entries) => {
                    let updates = entries
                        .iter()
                        .map(|entry| &entry.update)
                        .collect::<Vec<_>>();
                    let observations = observe_updates(&updates);
                    let mut paths = Vec::new();
                    for update in &updates {
                        if let SessionUpdate::FileEdit { path, .. } = update {
                            if !paths.iter().any(|p: &String| p == path) {
                                paths.push(path.clone());
                            }
                        }
                    }
                    let tests_passed = if observations.tests_observed == 0 {
                        None
                    } else if observations.tests_failed > 0 || observations.tests_incomplete > 0 {
                        Some(false)
                    } else if observations.tests_passed > 0 {
                        Some(true)
                    } else {
                        None
                    };
                    (observations, paths, tests_passed)
                }
                Err(_) => (CompletionObservations::default(), Vec::new(), None),
            };
        // Fall back to host-recorded edits when the journal page expired or
        // FileEdit events were filtered out of the turn window.
        if changed_paths.is_empty() {
            let g = self.inner.lock();
            for path in &g.edited_files {
                if !changed_paths.iter().any(|p| p == path) {
                    changed_paths.push(path.clone());
                }
            }
        }
        let usage_after = self.session_usage_snapshot(session_id);
        let usage = CompletionUsage {
            prompt_tokens: usage_after.0.saturating_sub(usage_before.0),
            completion_tokens: usage_after.1.saturating_sub(usage_before.1),
            total_tokens: usage_after.2.saturating_sub(usage_before.2),
            requests: usage_after.3.saturating_sub(usage_before.3),
        };
        // Enrich weak model finals with observed paths/test outcomes so the
        // terminal handoff and transcript always report what actually happened.
        let result = match result {
            Ok(reply) => {
                let incomplete = is_incomplete_stop_message(&reply);
                let enriched =
                    enrich_terminal_handoff(&reply, &changed_paths, tests_passed, incomplete);
                if enriched != reply {
                    // Always rewrite the last assistant line so transcript
                    // consumers (live_eval handoff) see the evidence trailer.
                    self.replace_last_assistant_text(session_id, &enriched);
                    // Append-only JSONL cannot mutate prior lines — rewrite so
                    // disk reload agrees with memory.
                    self.persist_session_rewrite(session_id);
                    let trailer = enriched
                        .strip_prefix(reply.trim())
                        .map(str::trim_start)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("");
                    if !trailer.is_empty() {
                        emit_message(&event_tx, session_id, trailer);
                    }
                }
                Ok(enriched)
            }
            Err(e) => Err(e),
        };
        let outcome = if duration_limited {
            "max_duration_reached"
        } else if cancelled {
            "cancelled"
        } else if let Some(code) = durable_stop_code.as_deref() {
            code
        } else if result.is_err() {
            "failed"
        } else {
            "completed"
        };
        let evidence = build_evidence(
            outcome,
            result.as_ref().ok().map(String::as_str),
            observations,
            usage,
            cancelled,
        );
        if let Err(error) = self.record_completion_evidence(session_id, turn_id, evidence.clone()) {
            eprintln!("[grokptah] completion evidence persist failed: {error:#}");
        }
        let _ = event_tx.send(SessionUpdate::CompletionEvidence {
            session_id,
            turn_id,
            evidence: evidence.clone(),
        });
        if let Some((run_id, store)) = desktop_run.as_ref() {
            if let Some(aggregator) = desktop_aggregator.take() {
                aggregator.abort();
                let _ = aggregator.await;
            }
            self.finalize_desktop_run(
                run_id, store, session_id, end_seq, &result, outcome, &evidence, &event_tx,
            )
            .await;
        }
        let final_result = match result {
            Ok(reply) => {
                let _ = event_tx.send(SessionUpdate::TurnComplete {
                    session_id,
                    cancelled,
                });
                Ok(reply)
            }
            Err(e) => {
                let _ = event_tx.send(SessionUpdate::Error {
                    session_id,
                    message: e.to_string(),
                });
                let _ = event_tx.send(SessionUpdate::TurnComplete {
                    session_id,
                    cancelled,
                });
                Err(e)
            }
        };

        // Keep the turn busy through the terminal event. A waiter observing an
        // idle session therefore knows model work and terminal fan-out ended.
        let (deferred, entries, revision) = {
            let mut g = self.inner.lock();
            g.turn_cancels.remove(&session_id);
            g.turn_max_rounds.remove(&session_id);
            let queue = g.prompt_queues.entry(session_id).or_default();
            let deferred = queue.defer_pending_steering();
            let entries = queue.list();
            let revision = (deferred > 0).then(|| g.next_queue_revision(session_id));
            (deferred, entries, revision)
        };
        let _ = self.persist_prompt_queue(session_id);
        if deferred > 0 {
            if let Some(revision) = revision {
                self.emit_prompt_queue_changed(
                    session_id,
                    revision,
                    entries,
                    "deferred",
                    "bridge".into(),
                    None,
                    Some(SteeringDisposition::Queued),
                );
            }
        }
        busy_guard.armed = false;
        final_result
    }

    fn ensure_build_workspace_ready(&self, session_id: Uuid) -> Result<()> {
        let (kind, cwd) = {
            let g = self.inner.lock();
            let session = g
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            (session.kind, session.cwd.clone())
        };
        if kind == SessionKind::Build {
            match workspace_status(&cwd) {
                WorkspaceStatus::Ready => {}
                status => bail!(
                    "session workspace is {}: {}; choose a working directory before sending a prompt",
                    status.as_str(),
                    cwd.display()
                ),
            }
        }
        Ok(())
    }

    fn persist_prompt_queue(&self, session_id: Uuid) -> Result<()> {
        let write = self.durable_write("persisting a prompt queue")?;
        let queue = {
            let g = self.inner.lock();
            g.prompt_queues.get(&session_id).cloned()
        };
        if let Some(q) = queue {
            session_store::save_prompt_queue(&write, session_id, &q)
                .map_err(|e| anyhow!("persist prompt queue: {e}"))?;
        } else {
            session_store::save_prompt_queue(&write, session_id, &SessionPromptQueue::default())
                .map_err(|e| anyhow!("persist prompt queue: {e}"))?;
        }
        Ok(())
    }

    fn emit_pending_steering_recovery(&self, session_id: Uuid, recovery: PromptQueueRecovery) {
        self.emit_prompt_queue_changed(
            session_id,
            recovery.revision,
            recovery.entries,
            "recovered",
            "bridge".into(),
            None,
            Some(SteeringDisposition::Queued),
        );
    }

    /// Publish a queue snapshot. `revision` must have been stamped by
    /// [`Inner::next_queue_revision`] under the same lock that committed the
    /// mutation — publishing happens here, after that lock is released, so
    /// `seq` order can invert and only `revision` orders these snapshots.
    #[allow(clippy::too_many_arguments)]
    fn emit_prompt_queue_changed(
        &self,
        session_id: Uuid,
        revision: u64,
        entries: Vec<PromptQueueEntry>,
        action: &str,
        origin: String,
        changed_entry: Option<PromptQueueEntry>,
        disposition: Option<SteeringDisposition>,
    ) {
        let event_tx = self.inner.lock().event_tx.clone();
        let _ = event_tx.send(SessionUpdate::PromptQueueChanged {
            session_id,
            revision,
            entries,
            action: action.into(),
            origin,
            changed_entry,
            disposition,
        });
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_turn(
        &self,
        session_id: Uuid,
        cwd: &Path,
        model: &str,
        effort: EffortLevel,
        plan_mode: bool,
        kind: SessionKind,
        prompt: &str,
        cancel: CancellationToken,
        event_tx: crate::event_bus::EventBus,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }

        let lower = prompt.to_lowercase();

        // ── Regular Grok chat: conversational only (no tool loop) ─────────
        if kind == SessionKind::Chat {
            if let Some(rest) = prompt.strip_prefix('/') {
                let cmd = rest.split_whitespace().next().unwrap_or("");
                if cmd == "help" {
                    let text = "Chat mode: plain conversation with Grok. Use Builds for coding tools. /help";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
            }
            let (wire_messages, compacted_summary) = {
                let g = self.inner.lock();
                let s = g
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("unknown session"))?;
                (api_context_messages(s), s.compacted_summary.clone())
            };
            let reply = if let Some(creds) =
                crate::auth_store::resolve_wire_credentials_for_model(model)
                    .map_err(anyhow::Error::msg)?
            {
                match call_xai_chat(
                    &creds,
                    model,
                    &wire_messages,
                    compacted_summary.as_deref(),
                    cwd,
                    SessionKind::Chat,
                )
                .await
                {
                    Ok(reply) => {
                        self.record_provider_usage(session_id, reply.usage.as_ref())?;
                        reply.text
                    }
                    Err(e) => format!(
                        "Model call failed: {e}\n\nAuth: {} ({})\nRun `grok login` if needed.",
                        creds.display_name, creds.method
                    ),
                }
            } else {
                "No credentials. Run `grok login` or save an API key to chat.".into()
            };
            // One message event for live UI; invoke return is the finalize
            // source of truth (SessionPane strips streamed assistants).
            emit_message(&event_tx, session_id, &reply);
            push_assistant(self, session_id, &reply);
            return Ok(reply);
        }

        // Plan mode: propose structured steps (model-backed when online).
        if plan_mode || lower.starts_with("/plan") || lower.contains("make a plan") {
            let goal = prompt
                .strip_prefix('/')
                .and_then(|r| r.strip_prefix("plan"))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(prompt)
                .trim()
                .to_string();
            let mut plan_token_stop = None;
            let steps = if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
                offline_plan_steps(&goal)
            } else if let Some(creds) = crate::auth_store::resolve_wire_credentials_for_model(model)
                .map_err(anyhow::Error::msg)?
            {
                let usage_attempt = match self.begin_provider_attempt(session_id).await {
                    Ok(attempt) => attempt,
                    Err(error) => {
                        plan_token_stop = self.run_token_stop_before_request(session_id);
                        if plan_token_stop.is_none() {
                            return Err(error);
                        }
                        None
                    }
                };
                if plan_token_stop.is_some() {
                    offline_plan_steps(&goal)
                } else {
                    match propose_plan_with_model(&creds, model, cwd, &goal, &cancel).await {
                        Ok((steps, usage)) if !steps.is_empty() => {
                            plan_token_stop = self.finish_provider_attempt(
                                session_id,
                                usage_attempt,
                                usage.as_ref(),
                            )?;
                            steps
                        }
                        Ok((_steps, usage)) => {
                            plan_token_stop = self.finish_provider_attempt(
                                session_id,
                                usage_attempt,
                                usage.as_ref(),
                            )?;
                            offline_plan_steps(&goal)
                        }
                        Err(e) => {
                            plan_token_stop =
                                self.finish_provider_attempt(session_id, usage_attempt, None)?;
                            let mut s = offline_plan_steps(&goal);
                            s.insert(0, format!("(model plan fallback: {e})"));
                            s
                        }
                    }
                }
            } else {
                offline_plan_steps(&goal)
            };

            {
                let mut g = self.inner.lock();
                if let Some(s) = g.sessions.get_mut(&session_id) {
                    s.plan_mode = true;
                    s.plan_steps = steps.clone();
                    s.plan_status = "proposed".into();
                    s.plan_goal = Some(goal.clone());
                }
            }
            let _ = event_tx.send(SessionUpdate::Plan {
                session_id,
                steps: steps.clone(),
                status: "proposed".into(),
            });
            let mut msg = String::from("Plan proposed. Accept or reject from the plan panel.\n\n");
            for (i, step) in steps.iter().enumerate() {
                msg.push_str(&format!("{}. {}\n", i + 1, step));
            }
            if let Some(stop) = plan_token_stop {
                msg.push('\n');
                msg.push_str(&stop);
            }
            emit_message(&event_tx, session_id, &msg);
            push_assistant(self, session_id, &msg);
            return Ok(msg);
        }

        if let Some(rest) = prompt.strip_prefix('/') {
            let mut parts = rest.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            let args: Vec<&str> = parts.collect();
            match cmd {
                "help" => {
                    let text = "Commands: /help /compact /plan [goal] /yolo /model [id] \
                         /effort [none|low|medium|high|max] /clear /context /mcp /skills \
                         /sandbox [read-only|workspace-write|full] (tool safety profile — \
                         not an OS sandbox) /explore [query] /agents /personas /usage.\n\
                         Build mode: multi-step tool loop + optional plan accept→execute.";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                "usage" => {
                    let u = {
                        let g = self.inner.lock();
                        g.session_usage
                            .get(&session_id)
                            .cloned()
                            .unwrap_or_default()
                    };
                    // Cost: document unknown rates; report tokens only (#159).
                    let text = format!(
                        "Session usage (#159):\n\
                         - requests: {}\n\
                         - prompt_tokens: {}\n\
                         - completion_tokens: {}\n\
                         - total_tokens: {}\n\
                         Cost: not computed (no fixed rate table; see /usage in Grok Build for billed estimates).",
                        u.requests, u.prompt_tokens, u.completion_tokens, u.total_tokens
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "yolo" => {
                    self.set_always_approve(true);
                    let text = "Always-approve enabled.";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                "compact" => {
                    let before = {
                        let g = self.inner.lock();
                        g.sessions
                            .get(&session_id)
                            .map(|s| s.transcript.len())
                            .unwrap_or(0)
                    };
                    let _ = self.compact_session_async(session_id).await?;
                    let after = {
                        let g = self.inner.lock();
                        g.sessions
                            .get(&session_id)
                            .map(|s| s.transcript.len())
                            .unwrap_or(0)
                    };
                    let text = format!(
                        "Context compacted for the server. Full local history retained \
                         (local messages {before} → {after}, never decreased)."
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "model" => {
                    if let Some(id) = args.first() {
                        self.set_model((*id).to_string());
                        let text = format!("Model set to `{id}`.");
                        emit_message(&event_tx, session_id, &text);
                        push_assistant(self, session_id, &text);
                        return Ok(text);
                    }
                    let cur = self.inner.lock().model.clone();
                    let text = format!("Current model: `{cur}`. Usage: /model <id>");
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "effort" => {
                    if let Some(raw) = args.first() {
                        let e = parse_effort_arg(raw);
                        self.set_effort(e);
                        let text = format!("Effort set to `{}`.", e.as_str());
                        emit_message(&event_tx, session_id, &text);
                        push_assistant(self, session_id, &text);
                        return Ok(text);
                    }
                    let cur = self.inner.lock().effort;
                    let text = format!(
                        "Current effort: `{}`. Usage: /effort none|low|medium|high|max",
                        cur.as_str()
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "clear" => {
                    {
                        let mut g = self.inner.lock();
                        if let Some(s) = g.sessions.get_mut(&session_id) {
                            s.transcript.clear();
                            s.api_context_start = 0;
                            s.compacted_summary = None;
                            s.persisted_len = 0;
                            s.plan_mode = false;
                            s.plan_steps.clear();
                            s.plan_status.clear();
                            s.plan_goal = None;
                            s.updated_at = Utc::now();
                        }
                    }
                    self.persist_session_rewrite(session_id);
                    let text = "Session cleared (local transcript reset).";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                "context" | "cost" => {
                    let text = {
                        let g = self.inner.lock();
                        let s = g
                            .sessions
                            .get(&session_id)
                            .ok_or_else(|| anyhow!("unknown session"))?;
                        let total = s.transcript.len().max(s.persisted_len);
                        let window = total.saturating_sub(s.api_context_start);
                        format!(
                            "Context: {total} local messages · API window starts at index {} \
                             ({window} messages on wire) · model `{}` · effort `{}` · \
                             sandbox `{}` · compact summary: {} chars",
                            s.api_context_start,
                            g.model,
                            g.effort.as_str(),
                            g.sandbox_profile,
                            s.compacted_summary.as_ref().map(|c| c.len()).unwrap_or(0)
                        )
                    };
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "mcp" => {
                    let lines = self.mcp_doctor();
                    let servers = self.mcp_list();
                    let mut text = String::from("MCP servers:\n");
                    for s in &servers {
                        text.push_str(&format!(
                            "- {} [{}] enabled={} status={}\n",
                            s.name, s.transport, s.enabled, s.status
                        ));
                    }
                    if servers.is_empty() {
                        text.push_str("(none configured)\n");
                    }
                    text.push_str("\nDoctor:\n");
                    text.push_str(&lines.join("\n"));
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "skills" => {
                    let skills = self.skills();
                    let mut text = String::from("Skills:\n");
                    for s in &skills {
                        text.push_str(&format!("- **{}**: {}\n", s.name, s.description));
                    }
                    if skills.is_empty() {
                        text.push_str("(none discovered)\n");
                    }
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "agents" => {
                    let agents = self.list_agents();
                    let mut text = String::from("Agents (#164):\n");
                    for a in &agents {
                        text.push_str(&format!("- **{}**: {}\n", a.name, a.description));
                    }
                    if agents.is_empty() {
                        text.push_str(
                            "(none — add `.md` under `.grok/agents/` or `~/.grokptah/agents/`)\n",
                        );
                    }
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "personas" => {
                    let personas = self.list_personas();
                    let mut text =
                        String::from("Personas (#164) — spawn with kind `general-purpose@name`:\n");
                    for p in &personas {
                        text.push_str(&format!("- **{}**: {}\n", p.name, p.description));
                    }
                    if personas.is_empty() {
                        text.push_str(
                            "(none — add `.toml` under `.grok/personas/` or `~/.grokptah/personas/`)\n",
                        );
                    }
                    let n = self.running_subagent_count(Some(session_id));
                    if n > 0 {
                        text.push_str(&format!(
                            "\n({n} subagent{} still running)\n",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "sandbox" => {
                    // Slash alias kept for muscle memory; labeled as tool-safety
                    // profile — not an OS sandbox (#114).
                    if let Some(p) = args.first() {
                        let norm = normalize_sandbox_profile(p);
                        self.set_sandbox(norm.to_string());
                        let text = format!(
                            "Tool safety profile set to `{norm}` \
                             (agent soft gates only — not an OS sandbox)."
                        );
                        emit_message(&event_tx, session_id, &text);
                        push_assistant(self, session_id, &text);
                        return Ok(text);
                    }
                    let cur = self.inner.lock().sandbox_profile.clone();
                    let text = format!(
                        "Tool safety profile: `{cur}`.\n\
                         These are agent-side soft gates (substring denylists / \
                         tool write checks) — **not** an OS sandbox or isolation boundary.\n\
                         Profiles: `read-only` (block write tools + mutator substrings), \
                         `workspace-write` (edits allowed; block only crude escape patterns), \
                         `full` (no agent-side gates).\n\
                         Usage: /sandbox <profile>  (alias kept for compatibility)"
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "explore" => {
                    let query = if args.is_empty() {
                        "summarize project layout".to_string()
                    } else {
                        args.join(" ")
                    };
                    let summary = self
                        .run_explore_subagent(session_id, cwd, &query, &cancel, &event_tx)
                        .await?;
                    emit_message(&event_tx, session_id, &summary);
                    push_assistant(self, session_id, &summary);
                    return Ok(summary);
                }
                _ => {}
            }
        }

        // Offline / CI: no live model (tests set GROKPTAH_AGENT_OFFLINE=1).
        if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
            return self
                .run_offline_build_turn(session_id, cwd, prompt, &cancel, &event_tx)
                .await;
        }

        // ── Real multi-step coding agent (tool-calling loop) ─────────────
        let Some(creds) = crate::auth_store::resolve_wire_credentials_for_model(model)
            .map_err(anyhow::Error::msg)?
        else {
            let msg = format!(
                "{}\n\nYou said: {}\nProject: {}\nModel: {} · effort: {}",
                crate::auth_store::auth_help_message(),
                prompt.chars().take(200).collect::<String>(),
                cwd.display(),
                model,
                effort.as_str()
            );
            emit_message(&event_tx, session_id, &msg);
            push_assistant(self, session_id, &msg);
            return Ok(msg);
        };

        let (wire_history, compacted_summary) = {
            let g = self.inner.lock();
            let s = g
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            (api_context_messages(s), s.compacted_summary.clone())
        };

        match self
            .run_coding_agent_loop(
                session_id,
                cwd,
                model,
                effort,
                &creds,
                &wire_history,
                compacted_summary.as_deref(),
                &cancel,
                &event_tx,
            )
            .await
        {
            Ok(reply) => Ok(reply),
            Err(e) => {
                if let Err(persist_error) = self.recover_pending_steering_delivery(session_id) {
                    let _ = event_tx.send(SessionUpdate::Error {
                        session_id,
                        message: persist_error.to_string(),
                    });
                }
                let es = e.to_string();
                surface_rate_limit_or_error(&event_tx, session_id, &es);
                let msg = format!(
                    "Agent failed: {es}\n\nAuth: {} ({})\nProject: {}\n\
                     Tips: run `grok login` if needed. If rate limited, wait before retrying.",
                    creds.display_name,
                    creds.method,
                    cwd.display()
                );
                emit_message(&event_tx, session_id, &msg);
                push_assistant(self, session_id, &msg);
                Ok(msg)
            }
        }
    }

    fn recover_pending_steering_delivery(&self, session_id: Uuid) -> Result<()> {
        let write = self
            .durable_write("recovering pending steering delivery")
            .ok();
        let outcome = {
            let mut g = self.inner.lock();
            recover_pending_steering_locked(write.as_ref(), &mut g, session_id)
        };
        match outcome {
            PromptQueueRecoveryOutcome::Nothing => Ok(()),
            PromptQueueRecoveryOutcome::Committed(recovery) => {
                self.emit_pending_steering_recovery(session_id, recovery);
                Ok(())
            }
            // The recovery is already applied in memory; the error still
            // propagates so the caller reports it, as it always has.
            PromptQueueRecoveryOutcome::NotPersisted { error } => Err(error),
        }
    }

    /// Report a recovery that could not be made durable.
    ///
    /// Uses the same `SessionUpdate::Error` channel the agent-error path
    /// already uses for this failure, so both routes are observable the same
    /// way. Deliberately not a queue snapshot: the mutation is live but not
    /// committed, and publishing a revision for it would advance consumer
    /// watermarks past a state that may not survive a restart.
    fn report_steering_recovery_failure(&self, session_id: Uuid, error: &anyhow::Error) {
        let event_tx = { self.inner.lock().event_tx.clone() };
        let _ = event_tx.send(SessionUpdate::Error {
            session_id,
            message: error.to_string(),
        });
    }

    fn drain_pending_steering(
        &self,
        session_id: Uuid,
        event_tx: &crate::event_bus::EventBus,
    ) -> Vec<PromptQueueEntry> {
        // Steering is only consumed once its in-flight state is durable. With
        // no write authority the note stays queued rather than being delivered
        // and lost (#455).
        let write = match self.durable_write("delivering queued steering") {
            Ok(write) => write,
            Err(error) => {
                eprintln!("[grokptah] steering delivery refused: {error:#}");
                return Vec::new();
            }
        };
        let (entries, revisions) = match (|| -> Result<(Vec<PromptQueueEntry>, Vec<u64>)> {
            let mut g = self.inner.lock();
            let mut next = g
                .prompt_queues
                .get(&session_id)
                .cloned()
                .unwrap_or_default();
            let entries = next.drain_steering();
            if entries.is_empty() {
                return Ok((entries, Vec::new()));
            }
            // Persist the in-flight delivery state before exposing the note to
            // the model. The next completed boundary acknowledges it.
            session_store::save_prompt_queue(&write, session_id, &next)
                .map_err(|e| anyhow!("persist consumed steering: {e}"))?;
            g.prompt_queues.insert(session_id, next);
            if let Some(session) = g.sessions.get_mut(&session_id) {
                for entry in &entries {
                    session.transcript.push(TranscriptEntry::system(format!(
                        "Steering while running [{}]: {}",
                        entry.id, entry.text
                    )));
                    session.updated_at = Utc::now();
                }
            }
            // One revision per event we are about to publish, all stamped here
            // under the mutation lock so they stay ordered against concurrent
            // desktop/MCP mutations.
            let revisions = (0..entries.len())
                .map(|_| g.next_queue_revision(session_id))
                .collect();
            Ok((entries, revisions))
        })() {
            Ok(result) => result,
            Err(error) => {
                let _ = event_tx.send(SessionUpdate::Error {
                    session_id,
                    message: error.to_string(),
                });
                return Vec::new();
            }
        };
        self.persist_session(session_id);
        for (entry, revision) in entries.iter().zip(revisions) {
            let _ = event_tx.send(SessionUpdate::SteeringInjected {
                session_id,
                steering_id: entry.id.clone(),
                text: entry.text.clone(),
            });
            let _ = event_tx.send(SessionUpdate::PromptQueueChanged {
                session_id,
                revision,
                entries: self.session_queue_list(session_id).unwrap_or_default(),
                action: "delivered".into(),
                origin: entry.owner.clone().unwrap_or_else(|| "bridge".into()),
                changed_entry: Some(entry.clone()),
                disposition: Some(SteeringDisposition::Pending),
            });
        }
        entries
    }

    fn append_pending_steering_messages(
        &self,
        session_id: Uuid,
        event_tx: &crate::event_bus::EventBus,
        messages: &mut Vec<serde_json::Value>,
    ) -> usize {
        let entries = self.drain_pending_steering(session_id, event_tx);
        for entry in &entries {
            messages.push(serde_json::json!({
                "role": "user",
                "content": format_interjection(&entry.text),
            }));
        }
        entries.len()
    }

    /// Deterministic Build turn for offline tests (no network).
    #[allow(clippy::too_many_arguments)]
    async fn run_offline_build_turn(
        &self,
        session_id: Uuid,
        cwd: &Path,
        prompt: &str,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String> {
        // Honor per-turn / host max_rounds so orchestration bounds are testable offline.
        let max_rounds = {
            let g = self.inner.lock();
            resolve_turn_max_rounds(
                g.turn_max_rounds.get(&session_id).copied(),
                g.max_agent_rounds,
            )
        };
        let lower = prompt.to_lowercase();
        // Explicit multi-round simulation: exhaust the wired budget and return the
        // same stop text the online coding loop emits (#196 round limits).
        if lower.contains("simulate_tool_rounds") {
            for round in 1..=max_rounds {
                if cancel.is_cancelled() {
                    return Ok("(cancelled)".into());
                }
                let _ = event_tx.send(SessionUpdate::AgentProgress {
                    session_id,
                    round: round as u32,
                    max_rounds: max_rounds as u32,
                    last_tool: Some("simulate".into()),
                    detail: format!("Offline simulate step {round}/{max_rounds}"),
                });
            }
            let msg = round_limit_stop_message(max_rounds);
            self.mark_run_stop(session_id, RunStopCause::RoundLimit, "max_rounds_reached")?;
            emit_message(event_tx, session_id, &msg);
            push_assistant(self, session_id, &msg);
            return Ok(msg);
        }
        if lower.contains("list") || lower.contains("files") || lower.contains("ls ") {
            let _ = self
                .run_tool_for_output(
                    session_id,
                    "list_dir",
                    &serde_json::json!({ "path": "." }),
                    || {
                        let cwd = cwd.to_path_buf();
                        async move { local_tools::tool_list_dir(&cwd, ".").await }
                    },
                    cancel,
                    event_tx,
                )
                .await;
        }
        // Offline read: "read path/to/file" — exercises tool_read_file + transcript.
        if let Some(path) = prompt
            .strip_prefix("read ")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.contains('\n'))
        {
            let _ = self
                .run_tool_for_output(
                    session_id,
                    "read_file",
                    &serde_json::json!({ "path": path }),
                    || {
                        let cwd = cwd.to_path_buf();
                        let path = path.clone();
                        async move { local_tools::tool_read_file(&cwd, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("write ") {
            if let Some((path, content)) = rest.split_once(':') {
                let path = path.trim().to_string();
                let content = content.trim().to_string();
                let path_rec = path.clone();
                if self.session_sandbox_is_readonly(session_id) {
                    let msg = "ERROR: tool safety profile is read-only; write_file denied";
                    emit_message(event_tx, session_id, msg);
                    // still finish turn below
                } else {
                    let out = self
                        .run_tool_for_output(
                            session_id,
                            "write_file",
                            &serde_json::json!({ "path": path, "content": content }),
                            || {
                                let cwd = cwd.to_path_buf();
                                let path = path.clone();
                                let content = content.clone();
                                async move {
                                    local_tools::tool_write_file(&cwd, &path, &content).await
                                }
                            },
                            cancel,
                            event_tx,
                        )
                        .await;
                    if out.as_ref().is_ok_and(|s| !s.starts_with("DENIED")) {
                        self.emit_file_edit(
                            session_id,
                            cwd,
                            &path_rec,
                            &format!("Wrote {path_rec}"),
                            event_tx,
                        );
                    }
                }
            }
        }
        if let Some(cmd) = prompt
            .strip_prefix("run ")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            let _ = self
                .run_shell_tool_for_output(session_id, cwd, &cmd, cancel, event_tx)
                .await;
        }
        // Offline todo: "todo add buy milk" or JSON after "todo "
        if let Some(rest) = prompt.strip_prefix("todo ") {
            let args = if rest.trim_start().starts_with('{') || rest.trim_start().starts_with('[') {
                serde_json::from_str(rest).unwrap_or_else(|_| {
                    serde_json::json!({
                        "todos": [{ "id": "1", "content": rest.trim(), "status": "pending" }]
                    })
                })
            } else {
                serde_json::json!({
                    "todos": [{ "id": "1", "content": rest.trim(), "status": "pending" }]
                })
            };
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "todo_write",
                    &args.to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("remember ") {
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "memory_write",
                    &serde_json::json!({
                        "text": rest.trim(),
                        "scope": { "kind": "project" }
                    })
                    .to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("recall ") {
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "memory_read",
                    &serde_json::json!({
                        "query": rest.trim(),
                        "scope": { "kind": "project" }
                    })
                    .to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("patch ") {
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "apply_patch",
                    &serde_json::json!({ "patch": rest.trim() }).to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if lower.starts_with("web_fetch ") {
            if let Some(url) = prompt.split_whitespace().nth(1) {
                let _ = self
                    .dispatch_agent_tool(
                        session_id,
                        cwd,
                        "web_fetch",
                        &serde_json::json!({ "url": url }).to_string(),
                        cancel,
                        event_tx,
                        &Default::default(),
                    )
                    .await;
            }
        }
        let steering = self.drain_pending_steering(session_id, event_tx);
        let steering_note = if steering.is_empty() {
            String::new()
        } else {
            format!(
                "\n[steering received: {}]",
                steering
                    .iter()
                    .map(|entry| entry.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        };
        // Prove wire context still carries compacted_summary after offline turns.
        let wire_note = {
            let g = self.inner.lock();
            g.sessions
                .get(&session_id)
                .and_then(|s| s.compacted_summary.as_ref())
                .map(|c| {
                    format!(
                        "\n[wire context includes compacted_summary: {} chars]",
                        c.len()
                    )
                })
                .unwrap_or_default()
        };
        let msg = format!(
            "(offline agent) done: {}{steering_note}{wire_note}",
            prompt.chars().take(80).collect::<String>()
        );
        emit_message(event_tx, session_id, &msg);
        push_assistant(self, session_id, &msg);
        Ok(msg)
    }

    /// Multi-round tool loop: model proposes tools → we run them → feed results
    /// back until a final text answer or max rounds.
    #[allow(clippy::too_many_arguments)]
    async fn run_coding_agent_loop(
        &self,
        session_id: Uuid,
        cwd: &Path,
        model: &str,
        effort: EffortLevel,
        creds: &crate::auth_store::WireCredentials,
        history: &[(String, String)],
        compacted_summary: Option<&str>,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String> {
        let max_rounds = {
            let g = self.inner.lock();
            resolve_turn_max_rounds(
                g.turn_max_rounds.get(&session_id).copied(),
                g.max_agent_rounds,
            )
        };
        // Auto-compact when wire window is large (non-destructive local history).
        {
            let need = {
                let g = self.inner.lock();
                g.sessions
                    .get(&session_id)
                    .map(|s| {
                        let window = s.transcript.len().saturating_sub(s.api_context_start);
                        window > 40
                    })
                    .unwrap_or(false)
            };
            if need {
                let _ = self.compact_session_async(session_id).await;
            }
        }

        let (active_plan, compacted_summary) = {
            let g = self.inner.lock();
            let s = g.sessions.get(&session_id);
            let plan = s.and_then(|s| {
                if matches!(s.plan_status.as_str(), "accepted" | "executing" | "done")
                    && !s.plan_steps.is_empty()
                {
                    Some((
                        s.plan_goal.clone().unwrap_or_else(|| "execute plan".into()),
                        s.plan_steps.clone(),
                    ))
                } else {
                    None
                }
            });
            let summary = s.and_then(|s| s.compacted_summary.clone());
            (
                plan,
                summary.or_else(|| compacted_summary.map(|s| s.to_string())),
            )
        };
        let plan_ref = active_plan
            .as_ref()
            .map(|(g, steps)| (g.as_str(), steps.as_slice()));
        let memory_access = self.memory_access_for_session(session_id)?;
        let mut messages = build_agent_messages(
            history,
            compacted_summary.as_deref(),
            cwd,
            Some(&memory_access),
            plan_ref,
        );

        // Best-effort MCP discovery (skipped when offline env set for tests).
        let mcp_specs = if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some()
            || std::env::var_os("GROKPTAH_MCP_SKIP").is_some()
        {
            Vec::new()
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(8),
                crate::mcp_runtime::list_mcp_tools(Some(cwd)),
            )
            .await
            .unwrap_or_default()
        };
        let (tools, mcp_index) = coding_agent_tools(&mcp_specs);
        // #168: at most one Stop-hook continue per user turn
        let mut stop_continued = false;
        let mut identical_tool_calls = IdenticalToolCallRun::default();
        let mut test_failure_needs_edit = false;
        // Cargo failures since last successful edit while armed.
        let mut cargo_fails_since_edit: u32 = 0;
        // Sticky for the turn: at least one successful edit after cargo went red.
        let mut had_edit_since_cargo_fail = false;
        // Distinct failing tests from the last cargo failure (multi-bug batching).
        let mut last_failure_count: u32 = 0;
        let mut recovery_grace = false;

        for round in 1..=max_rounds.saturating_add(1) {
            let in_recovery_grace = round > max_rounds;
            if in_recovery_grace && !recovery_grace {
                break;
            }
            let visible_max_rounds = if in_recovery_grace {
                max_rounds.saturating_add(1)
            } else {
                max_rounds
            };
            if cancel.is_cancelled() {
                let msg = "(cancelled)".to_string();
                emit_message(event_tx, session_id, &msg);
                push_assistant(self, session_id, &msg);
                return Ok(msg);
            }
            if let Some(msg) = self.run_token_stop_before_request(session_id) {
                emit_message(event_tx, session_id, &msg);
                push_assistant(self, session_id, &msg);
                return Ok(msg);
            }

            let steering_count =
                self.append_pending_steering_messages(session_id, event_tx, &mut messages);

            // Give an explicit steering prompt one model boundary to break a
            // stationary run before applying the automatic stop.
            if steering_count == 0 {
                if let Some((run_len, tool_name, true_noop)) = identical_tool_calls.stop_info() {
                    let msg = action_stationarity_stop_message(run_len, &tool_name, true_noop);
                    self.mark_run_stop(session_id, RunStopCause::Stationarity, "stationarity")?;
                    let _ = event_tx.send(SessionUpdate::AgentProgress {
                        session_id,
                        round: round as u32,
                        max_rounds: visible_max_rounds as u32,
                        last_tool: Some(tool_name),
                        detail: msg.clone(),
                    });
                    emit_message(event_tx, session_id, &msg);
                    push_assistant(self, session_id, &msg);
                    return Ok(msg);
                }
            }

            if identical_tool_calls.take_nudge() {
                let run_len = identical_tool_calls.run_len();
                let tool_name = identical_tool_calls.tool_name();
                let nudge = action_stationarity_nudge(&tool_name, run_len);
                let _ = event_tx.send(SessionUpdate::AgentProgress {
                    session_id,
                    round: round as u32,
                    max_rounds: visible_max_rounds as u32,
                    last_tool: Some(tool_name),
                    detail: nudge.clone(),
                });
                messages.push(serde_json::json!({
                    "role": "system",
                    "content": nudge,
                }));
            }

            // Budget-aware coaching when max_agent_rounds is tight (#187/#188/#223).
            let remaining = if in_recovery_grace {
                1
            } else {
                max_rounds.saturating_sub(round) + 1
            };
            // After any observed cargo failure under a tight budget, stop burning
            // steps on list/grep/read — force edit + shell until cargo is green.
            // After repeated cargo fails without an edit, force edit-only so the
            // model cannot thrash shell-only under max_turns=3 (#187 R2).
            let force_edit_shell =
                max_rounds <= 8 && (in_recovery_grace || remaining == 1 || test_failure_needs_edit);
            // While cargo is red and no edit has landed, advertise edit-only so
            // the model cannot thrash shell. After an edit, restore edit+shell
            // (host also auto-re-runs cargo after writes). With 2+ failures,
            // drop serial write_file so multi-module write_files is forced.
            let force_edit_only = max_rounds <= 8
                && test_failure_needs_edit
                && !had_edit_since_cargo_fail
                && !in_recovery_grace;
            let multi_failure_batch = last_failure_count >= 2;
            let tools_this_round = if force_edit_shell {
                let coach = if in_recovery_grace {
                    if multi_failure_batch {
                        format!(
                            "TEST RECOVERY: {last_failure_count} independent cargo failures remain. \
                             Use ONE write_files call covering every implicated module (not serial \
                             write_file), then cargo will re-verify. Do not stop at a diagnosis."
                        )
                    } else {
                        "TEST RECOVERY: the model budget ended with unresolved cargo test failures (or edits that were not re-verified). Use failures and source already in context. In this one bounded recovery step: apply any remaining fixes with write_files and re-run cargo test. Do not stop at a diagnosis or claim success without a green cargo test.".into()
                    }
                } else if force_edit_only && multi_failure_batch {
                    format!(
                        "BUDGET: {last_failure_count} independent test failures. Shell and serial \
                         write_file are disabled. Use ONE write_files (or multi-file apply_patch) \
                         fixing ALL modules now. cargo re-runs automatically after edits."
                    )
                } else if force_edit_only {
                    "BUDGET: cargo test failed. Shell is disabled until you edit. Use write_files (preferred — every failing module in ONE call) / write_file / apply_patch to fix ALL failures now. cargo test re-runs automatically after your edits.".into()
                } else if test_failure_needs_edit && multi_failure_batch {
                    format!(
                        "BUDGET: {last_failure_count} independent failures still open. Prefer ONE \
                         write_files batch for remaining modules; cargo re-runs after edits."
                    )
                } else if test_failure_needs_edit {
                    "BUDGET: cargo test has failed and is not green yet. Prefer write_files for remaining fixes; cargo re-runs automatically after edits.".into()
                } else {
                    "BUDGET: FINAL model step. Exploration tools are disabled. Use only write_files / write_file / apply_patch / run_terminal_cmd. Apply ALL remaining fixes in one batch (every failing test / complete rename including re-exports) and run cargo test now. For renames: change type identifiers only — never rewrite user-facing / PRODUCT_LABEL string literals.".into()
                };
                messages.push(serde_json::json!({
                    "role": "system",
                    "content": coach,
                }));
                if force_edit_only && multi_failure_batch {
                    filter_tools_batch_edit_only(&tools)
                } else if force_edit_only {
                    filter_tools_edit_only(&tools)
                } else {
                    filter_tools_edit_and_shell(&tools)
                }
            } else {
                if max_rounds <= 8 && remaining <= 2 {
                    messages.push(serde_json::json!({
                        "role": "system",
                        "content": "BUDGET: only 2 model steps left including this one. Prefer dense multi-tool edits (write_files for every implicated file) + cargo test over list/grep/read. Batch independent bugs; for renames preserve string literals.",
                    }));
                }
                tools.clone()
            };

            let _ = event_tx.send(SessionUpdate::AgentProgress {
                session_id,
                round: round as u32,
                max_rounds: visible_max_rounds as u32,
                last_tool: None,
                detail: format!("Model step {round}/{visible_max_rounds}"),
            });

            let usage_attempt = match self.begin_provider_attempt(session_id).await {
                Ok(attempt) => attempt,
                Err(error) => {
                    if let Some(stop) = self.run_token_stop_before_request(session_id) {
                        emit_message(event_tx, session_id, &stop);
                        push_assistant(self, session_id, &stop);
                        return Ok(stop);
                    }
                    return Err(error);
                }
            };
            let provider_observation = self.provider_observation_context(session_id);
            let step = match call_xai_agent_step_observed(
                creds,
                model,
                effort,
                &messages,
                &tools_this_round,
                !self.run_tokens_bounded(session_id),
                cancel,
                provider_observation.as_ref(),
                |delta| {
                    emit_message(event_tx, session_id, delta);
                },
                |thought| {
                    emit_thought(event_tx, session_id, thought);
                },
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    if let Some(stop) =
                        self.finish_provider_attempt(session_id, usage_attempt, None)?
                    {
                        emit_message(event_tx, session_id, &stop);
                        push_assistant(self, session_id, &stop);
                        return Ok(stop);
                    }
                    surface_rate_limit_or_error(event_tx, session_id, &e.to_string());
                    return Err(e);
                }
            };
            let token_stop = self.finish_provider_attempt(
                session_id,
                usage_attempt,
                match &step {
                    AgentStep::Final { usage, .. } | AgentStep::ToolCalls { usage, .. } => {
                        usage.as_ref()
                    }
                },
            )?;

            match step {
                AgentStep::Final {
                    mut text,
                    streamed,
                    reasoning,
                    ..
                } => {
                    if let Some(r) = reasoning.as_deref() {
                        push_thought(self, session_id, r);
                    }
                    if let Some(stop) = token_stop {
                        let original = text.trim().to_string();
                        if !original.is_empty() && !streamed {
                            emit_message(event_tx, session_id, &original);
                        }
                        emit_message(event_tx, session_id, &stop);
                        text = if original.is_empty() {
                            stop
                        } else {
                            format!("{original}\n\n{stop}")
                        };
                        push_assistant(self, session_id, &text);
                        return Ok(text);
                    }
                    if max_rounds <= 8 && test_failure_needs_edit {
                        if in_recovery_grace {
                            // Recovery already spent — do not accept a success claim
                            // while cargo is still unresolved (#187).
                            let msg = "Stopped after recovery step with unresolved cargo test failures. Ask me to continue with the failing tests and source still in context.".to_string();
                            self.mark_run_stop(
                                session_id,
                                RunStopCause::RecoveryExhausted,
                                "recovery_exhausted",
                            )?;
                            emit_message(event_tx, session_id, &msg);
                            push_assistant(self, session_id, &msg);
                            return Ok(msg);
                        }
                        messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": text,
                        }));
                        if round == max_rounds {
                            recovery_grace = true;
                        }
                        messages.push(serde_json::json!({
                            "role": "system",
                            "content": "TEST RECOVERY is incomplete. Do not provide a final answer yet. Apply the code edits required by ALL failing tests, then re-run cargo test until green.",
                        }));
                        continue;
                    }
                    let text = if text.trim().is_empty() {
                        if reasoning.as_ref().is_some_and(|r| !r.trim().is_empty()) {
                            // Reasoning-only turn already shown as thought; keep a thin marker.
                            String::new()
                        } else {
                            "(agent finished with empty reply)".into()
                        }
                    } else {
                        text
                    };
                    if !text.is_empty() {
                        if !streamed {
                            emit_message(event_tx, session_id, &text);
                        }
                        push_assistant(self, session_id, &text);
                    }
                    if round < max_rounds {
                        messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": text,
                        }));
                        if self.append_pending_steering_messages(
                            session_id,
                            event_tx,
                            &mut messages,
                        ) > 0
                        {
                            continue;
                        }
                    }
                    // #168 Stop hooks: optional continue with feedback (once).
                    if !stop_continued && !cancel.is_cancelled() {
                        match crate::hooks::evaluate_stop_hooks(Some(cwd)) {
                            crate::hooks::StopHookResult::ContinueWithFeedback(fb) => {
                                stop_continued = true;
                                let note = format!("(Stop hook — continuing with feedback)\n{fb}");
                                emit_message(event_tx, session_id, &note);
                                push_assistant(self, session_id, &note);
                                messages.push(serde_json::json!({
                                    "role": "user",
                                    "content": format!(
                                        "<system-reminder>Stop hook feedback: {fb}</system-reminder>"
                                    )
                                }));
                                continue;
                            }
                            crate::hooks::StopHookResult::End => {}
                        }
                    }
                    // #165: surface remaining subagents in the final line when any still run.
                    let still = self.running_subagent_count(Some(session_id));
                    let base = if text.is_empty() {
                        reasoning.unwrap_or_else(|| "(thought only)".into())
                    } else {
                        text
                    };
                    if still > 0 {
                        let note = if still == 1 {
                            format!("{base}\n\n(1 subagent still running)")
                        } else {
                            format!("{base}\n\n({still} subagents still running)")
                        };
                        return Ok(note);
                    }
                    return Ok(base);
                }
                AgentStep::ToolCalls {
                    content,
                    tool_calls,
                    streamed,
                    reasoning,
                    ..
                } => {
                    if let Some(r) = reasoning.as_deref() {
                        push_thought(self, session_id, r);
                    }
                    if !streamed {
                        if let Some(c) = content.as_ref().filter(|s| !s.trim().is_empty()) {
                            emit_message(event_tx, session_id, c);
                        }
                    }

                    // OpenAI-style assistant message carrying tool_calls
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls.iter().map(|tc| serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }
                        })).collect::<Vec<_>>(),
                    }));

                    let mut edited_while_needs_reverify = false;
                    for tc in &tool_calls {
                        if cancel.is_cancelled() {
                            break;
                        }
                        // Mid-batch gate (#187): once cargo has failed under a
                        // tight budget, do not burn remaining calls in this step
                        // (or later steps) on list/read/grep/glob exploration.
                        if should_skip_tool_after_cargo_failure(
                            max_rounds as u32,
                            test_failure_needs_edit,
                            &tc.name,
                            had_edit_since_cargo_fail,
                        ) {
                            let content = post_cargo_failure_skip_message(&tc.name);
                            messages.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tc.id,
                                "content": content,
                            }));
                            continue;
                        }
                        let _ = event_tx.send(SessionUpdate::AgentProgress {
                            session_id,
                            round: round as u32,
                            max_rounds: visible_max_rounds as u32,
                            last_tool: Some(tc.name.clone()),
                            detail: format!("Tool `{}` (round {round})", tc.name),
                        });
                        let output = self
                            .dispatch_agent_tool(
                                session_id,
                                cwd,
                                &tc.name,
                                &tc.arguments,
                                cancel,
                                event_tx,
                                &mcp_index,
                            )
                            .await;
                        let content = match &output {
                            Ok(s) => s.clone(),
                            Err(e) => format!("ERROR: {e}"),
                        };
                        // Cap tool output size for the wire
                        let content = if content.len() > 24_000 {
                            let orig_len = content.len();
                            format!(
                                "{}…\n(truncated {} bytes)",
                                crate::textutil::truncate_at_char_boundary(&content, 24_000),
                                orig_len
                            )
                        } else {
                            content
                        };
                        // Under tight budgets, only clear the post-failure gate when cargo is
                        // green again. Clearing on edit alone allowed final answers without a
                        // re-run (#187 verified=false despite oracle pass via external check).
                        if max_rounds <= 8 && tc.name == "run_terminal_cmd" {
                            if cargo_test_output_failed(&content) {
                                test_failure_needs_edit = true;
                                cargo_fails_since_edit = cargo_fails_since_edit.saturating_add(1);
                                let n = count_cargo_test_failures(&content);
                                if n > 0 {
                                    last_failure_count = n;
                                } else {
                                    last_failure_count = last_failure_count.max(1);
                                }
                                messages.push(serde_json::json!({
                                    "role": "system",
                                    "content": cargo_test_failure_coaching(&content),
                                }));
                            } else if cargo_test_output_passed(&content) {
                                test_failure_needs_edit = false;
                                cargo_fails_since_edit = 0;
                                last_failure_count = 0;
                            }
                        }
                        if max_rounds <= 8
                            && test_failure_needs_edit
                            && output.is_ok()
                            && matches!(
                                tc.name.as_str(),
                                "write_files" | "write_file" | "apply_patch"
                            )
                            && !content.starts_with("ERROR:")
                            && !content.starts_with("DENIED")
                        {
                            cargo_fails_since_edit = 0;
                            had_edit_since_cargo_fail = true;
                            edited_while_needs_reverify = true;
                            if last_failure_count >= 2 && tc.name == "write_file" {
                                messages.push(serde_json::json!({
                                    "role": "system",
                                    "content": multi_failure_partial_edit_coaching(last_failure_count),
                                }));
                            }
                            messages.push(serde_json::json!({
                                "role": "system",
                                "content": cargo_test_reverify_coaching(),
                            }));
                        }
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": content,
                        }));
                    }

                    // Host-driven cargo re-verify after edits while still red
                    // under tight budgets (#187). Ensures a final write is always
                    // followed by cargo so verified can go green even when the
                    // model spends the last step on write_files only.
                    if should_auto_cargo_reverify_after_edit(
                        max_rounds as u32,
                        edited_while_needs_reverify,
                    ) && !cancel.is_cancelled()
                    {
                        let cmd = auto_cargo_reverify_command();
                        let args = serde_json::json!({ "command": cmd }).to_string();
                        let reverify_id = format!("auto-reverify-{}", Uuid::new_v4());
                        let _ = event_tx.send(SessionUpdate::AgentProgress {
                            session_id,
                            round: round as u32,
                            max_rounds: visible_max_rounds as u32,
                            last_tool: Some("run_terminal_cmd".into()),
                            detail: format!(
                                "Auto re-verify `cargo test` after edit (round {round})"
                            ),
                        });
                        // Synthetic assistant tool_call so the wire transcript is coherent.
                        messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": reverify_id,
                                "type": "function",
                                "function": {
                                    "name": "run_terminal_cmd",
                                    "arguments": args,
                                }
                            }],
                        }));
                        let output = self
                            .dispatch_agent_tool(
                                session_id,
                                cwd,
                                "run_terminal_cmd",
                                &args,
                                cancel,
                                event_tx,
                                &mcp_index,
                            )
                            .await;
                        let content = match &output {
                            Ok(s) => s.clone(),
                            Err(e) => format!("ERROR: {e}"),
                        };
                        let content = if content.len() > 24_000 {
                            let orig_len = content.len();
                            format!(
                                "{}…\n(truncated {} bytes)",
                                crate::textutil::truncate_at_char_boundary(&content, 24_000),
                                orig_len
                            )
                        } else {
                            content
                        };
                        if cargo_test_output_failed(&content) {
                            test_failure_needs_edit = true;
                            cargo_fails_since_edit = cargo_fails_since_edit.saturating_add(1);
                            let n = count_cargo_test_failures(&content);
                            if n > 0 {
                                last_failure_count = n;
                            } else {
                                last_failure_count = last_failure_count.max(1);
                            }
                            // Partial multi-file fix: require another batch edit.
                            had_edit_since_cargo_fail = false;
                            messages.push(serde_json::json!({
                                "role": "system",
                                "content": cargo_test_failure_coaching(&content),
                            }));
                        } else if cargo_test_output_passed(&content) {
                            test_failure_needs_edit = false;
                            cargo_fails_since_edit = 0;
                            last_failure_count = 0;
                            messages.push(serde_json::json!({
                                "role": "system",
                                "content": "Auto re-verify: cargo test passed after edits.",
                            }));
                        }
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": reverify_id,
                            "content": content,
                        }));
                    }

                    // Preserve normal inspection after an early failure. If the
                    // budget ends without an edit, arm exactly one edit-only pass.
                    if max_rounds <= 8 && round == max_rounds && test_failure_needs_edit {
                        recovery_grace = true;
                    }

                    let signature = tool_step_signature(&tool_calls);
                    let tool_name = tool_calls
                        .first()
                        .map(|tool_call| tool_call.name.as_str())
                        .unwrap_or("");
                    identical_tool_calls.observe(
                        &signature,
                        tool_name,
                        is_true_noop_tool_step(&tool_calls),
                    );
                    // Usage belongs to the model boundary that produced these
                    // calls. Let every tool in that accepted response settle,
                    // then stop here so the final loop exit cannot overwrite a
                    // token-ceiling cause with a round-limit cause.
                    if let Some(stop) = token_stop {
                        emit_message(event_tx, session_id, &stop);
                        push_assistant(self, session_id, &stop);
                        return Ok(stop);
                    }
                }
            }
        }

        // A synchronous tool (notably spawn_explore) may have completed a
        // shared bounded provider attempt after the parent response usage was
        // recorded. Re-read the shared tracker before installing a round-limit
        // cause so that child usage remains authoritative.
        if let Some(msg) = self.run_token_stop_before_request(session_id) {
            emit_message(event_tx, session_id, &msg);
            push_assistant(self, session_id, &msg);
            return Ok(msg);
        }

        let msg = if recovery_grace {
            recovery_round_limit_stop_message(max_rounds)
        } else {
            round_limit_stop_message(max_rounds)
        };
        self.mark_run_stop(
            session_id,
            if recovery_grace {
                RunStopCause::RecoveryExhausted
            } else {
                RunStopCause::RoundLimit
            },
            if recovery_grace {
                "recovery_exhausted"
            } else {
                "max_rounds_reached"
            },
        )?;
        debug_assert!(is_round_limit_stop_message(&msg));
        emit_message(event_tx, session_id, &msg);
        push_assistant(self, session_id, &msg);
        Ok(msg)
    }

    /// Emit live diff update after a successful edit tool.
    fn emit_file_edit(
        &self,
        session_id: Uuid,
        cwd: &Path,
        path: &str,
        summary: &str,
        event_tx: &crate::event_bus::EventBus,
    ) {
        self.record_edit(path);
        let unified = crate::project_context::diff_for_path(cwd, path);
        let _ = event_tx.send(SessionUpdate::FileEdit {
            session_id,
            path: path.to_string(),
            summary: summary.to_string(),
            unified_diff: unified,
        });
    }

    /// Run one model-requested tool with permissions + UI events.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_agent_tool(
        &self,
        session_id: Uuid,
        cwd: &Path,
        name: &str,
        arguments_json: &str,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
        mcp_index: &McpToolIndex,
    ) -> Result<String> {
        let args: serde_json::Value = serde_json::from_str(arguments_json)
            .context("model returned malformed tool arguments")?;
        if !args.is_object() {
            bail!("model tool arguments must be a JSON object");
        }

        // Namespaced MCP tools
        if let Some((server, tool)) = mcp_index.get(name) {
            return self
                .run_mcp_tool(session_id, cwd, server, tool, name, &args, cancel, event_tx)
                .await;
        }

        match name {
            "list_dir" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".")
                    .to_string();
                self.run_tool_for_output(
                    session_id,
                    "list_dir",
                    &args,
                    || {
                        let cwd = cwd.to_path_buf();
                        let path = path.clone();
                        async move { local_tools::tool_list_dir(&cwd, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("read_file requires path"))?
                    .to_string();
                self.run_tool_for_output(
                    session_id,
                    "read_file",
                    &args,
                    || {
                        let cwd = cwd.to_path_buf();
                        let path = path.clone();
                        async move { local_tools::tool_read_file(&cwd, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            "grep" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("grep requires pattern"))?
                    .to_string();
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".")
                    .to_string();
                self.run_tool_for_output(
                    session_id,
                    "grep",
                    &args,
                    || {
                        let cwd = cwd.to_path_buf();
                        let pattern = pattern.clone();
                        let path = path.clone();
                        async move { local_tools::tool_grep(&cwd, &pattern, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            "write_file" => {
                if self.session_sandbox_is_readonly(session_id) {
                    return Ok("ERROR: tool safety profile is read-only; write_file denied".into());
                }
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("write_file requires path"))?
                    .to_string();
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("write_file requires content"))?
                    .to_string();
                let path_record = path.clone();
                // #146: snapshot original *before* write so FilesOnly rewind can restore.
                self.snapshot_edit_original_for_session(session_id, cwd, &path_record);
                let out = self
                    .run_tool_for_output(
                        session_id,
                        "write_file",
                        &args,
                        || {
                            let cwd = cwd.to_path_buf();
                            let path = path.clone();
                            let content = content.clone();
                            async move { local_tools::tool_write_file(&cwd, &path, &content).await }
                        },
                        cancel,
                        event_tx,
                    )
                    .await;
                if let Ok(ref report) = out {
                    self.emit_file_edit(session_id, cwd, &path_record, report, event_tx);
                }
                out
            }
            "write_files" => {
                if self.session_sandbox_is_readonly(session_id) {
                    return Ok("ERROR: tool safety profile is read-only; write_files denied".into());
                }
                let files_val = args
                    .get("files")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| anyhow!("write_files requires files array"))?;
                let mut files: Vec<(String, String)> = Vec::new();
                for (i, item) in files_val.iter().enumerate() {
                    let path = item
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("write_files[{i}].path required"))?
                        .to_string();
                    let content = item
                        .get("content")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("write_files[{i}].content required"))?
                        .to_string();
                    self.snapshot_edit_original_for_session(session_id, cwd, &path);
                    files.push((path, content));
                }
                if files.is_empty() {
                    return Ok("ERROR: write_files files array is empty".into());
                }
                let paths: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
                let out = self
                    .run_tool_for_output(
                        session_id,
                        "write_files",
                        &args,
                        || {
                            let cwd = cwd.to_path_buf();
                            let files = files.clone();
                            async move { local_tools::tool_write_files(&cwd, &files).await }
                        },
                        cancel,
                        event_tx,
                    )
                    .await;
                if let Ok(ref report) = out {
                    for p in &paths {
                        self.emit_file_edit(session_id, cwd, p, report, event_tx);
                    }
                }
                out
            }
            "run_terminal_cmd" => {
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("run_terminal_cmd requires command"))?
                    .to_string();
                if self.session_sandbox_blocks_shell(session_id, &command) {
                    return Ok(format!(
                        "ERROR: tool safety profile forbids this shell command \
                         (soft denylist, not an OS sandbox): {command}"
                    ));
                }
                self.run_shell_tool_for_output(session_id, cwd, &command, cancel, event_tx)
                    .await
            }
            "glob_files" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("glob_files requires pattern"))?
                    .to_string();
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
                let hits = crate::project_context::glob_files(cwd, &pattern, limit);
                let out = if hits.is_empty() {
                    "(no matches)".into()
                } else {
                    hits.join("\n")
                };
                // Emit a lightweight tool card for the UI
                let call_id = Uuid::new_v4().to_string();
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "glob_files".into(),
                    kind: ToolCallKind::Search,
                    status: ToolCallStatus::Running,
                    input: args.clone(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Completed,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "glob_files",
                    ToolCallStatus::Completed,
                    Some(out.clone()),
                );
                Ok(out)
            }
            "apply_patch" => {
                if self.session_sandbox_is_readonly(session_id) {
                    return Ok(
                        "ERROR: tool safety profile is read-only; apply_patch denied".into(),
                    );
                }
                let patch = args
                    .get("patch")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.get("content").and_then(|v| v.as_str()))
                    .ok_or_else(|| anyhow!("apply_patch requires patch"))?
                    .to_string();
                let input = args.clone();
                let needs = true;
                let gate = self.tool_gate(session_id, "apply_patch");
                if gate == ToolGate::AutoDeny {
                    return Ok("DENIED by deny rule: apply_patch".into());
                }
                let always = matches!(gate, ToolGate::AutoAllow);
                let call_id = Uuid::new_v4().to_string();
                if needs && !always {
                    let decision = self
                        .prompt_tool_permission(
                            session_id,
                            "apply_patch",
                            "Allow apply_patch (edit files)?".into(),
                            input.clone(),
                            cancel,
                        )
                        .await;
                    if decision == PermissionDecision::Deny {
                        let _ = event_tx.send(SessionUpdate::ToolCall {
                            session_id,
                            call_id: call_id.clone(),
                            title: "apply_patch".into(),
                            kind: ToolCallKind::Edit,
                            status: ToolCallStatus::Denied,
                            input,
                        });
                        return Ok("DENIED: user denied apply_patch".into());
                    }
                    if decision == PermissionDecision::AlwaysAllow {
                        let mut g = self.inner.lock();
                        g.always_allowed_tools.insert("apply_patch".into());
                    }
                }
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "apply_patch".into(),
                    kind: ToolCallKind::Edit,
                    status: ToolCallStatus::Running,
                    input,
                });
                // Best-effort path hints from patch text for pre-edit snapshots (#146).
                for line in patch.lines() {
                    if let Some(p) = line
                        .strip_prefix("*** Update File: ")
                        .or_else(|| line.strip_prefix("*** Add File: "))
                    {
                        let path = p.trim();
                        if !path.is_empty() {
                            self.snapshot_edit_original_for_session(session_id, cwd, path);
                        }
                    }
                }
                match crate::project_context::apply_patch(cwd, &patch) {
                    Ok(report) => {
                        // Record + live-diff every path in the report
                        for line in report.lines() {
                            if let Some(p) = line.strip_prefix("updated ") {
                                let path = p.split(' ').next().unwrap_or("");
                                if !path.is_empty() {
                                    self.emit_file_edit(session_id, cwd, path, line, event_tx);
                                }
                            }
                        }
                        let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                            session_id,
                            call_id: call_id.clone(),
                            status: ToolCallStatus::Completed,
                            output: Some(report.clone()),
                        });
                        push_tool(
                            self,
                            session_id,
                            &call_id,
                            "apply_patch",
                            ToolCallStatus::Completed,
                            Some(report.clone()),
                        );
                        Ok(report)
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                            session_id,
                            call_id: call_id.clone(),
                            status: ToolCallStatus::Failed,
                            output: Some(msg.clone()),
                        });
                        push_tool(
                            self,
                            session_id,
                            &call_id,
                            "apply_patch",
                            ToolCallStatus::Failed,
                            Some(msg.clone()),
                        );
                        Ok(format!("ERROR: {msg}"))
                    }
                }
            }
            "spawn_explore" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("explore the codebase")
                    .to_string();
                self.run_explore_subagent(session_id, cwd, &query, cancel, event_tx)
                    .await
            }
            "spawn_general_purpose" | "spawn_subagent" => {
                let prompt = args
                    .get("prompt")
                    .or_else(|| args.get("query"))
                    .or_else(|| args.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("complete the delegated task")
                    .to_string();
                let kind = args
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("general-purpose")
                    .to_string();
                // Fire-and-forget: returns immediately so multiple children run in parallel (#151).
                self.spawn_gp_subagent_parallel(session_id, cwd, &prompt, &kind, cancel, event_tx)
            }
            "todo_write" => {
                let (items, merge) =
                    crate::todo_list::TodoList::from_tool_args(&args).map_err(|e| anyhow!(e))?;
                let rendered = {
                    let mut g = self.inner.lock();
                    let s = g
                        .sessions
                        .get_mut(&session_id)
                        .ok_or_else(|| anyhow!("unknown session"))?;
                    s.todos.apply_update(items, merge);
                    s.todos.render()
                };
                let call_id = Uuid::new_v4().to_string();
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "todo_write".into(),
                    kind: ToolCallKind::Think,
                    status: ToolCallStatus::Running,
                    input: args.clone(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Completed,
                    output: Some(rendered.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "todo_write",
                    ToolCallStatus::Completed,
                    Some(rendered.clone()),
                );
                Ok(rendered)
            }
            "memory_write" => {
                let text = args
                    .get("text")
                    .or_else(|| args.get("fact"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("memory_write requires text"))?
                    .to_string();
                let tags: Vec<String> = args
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let address = self.memory_address_from_args(session_id, &args)?;
                let self_for_memory = self.clone();
                self.run_tool_for_output(
                    session_id,
                    "memory_write",
                    &args,
                    || async move {
                        let write = self_for_memory.durable_write("writing a memory fact")?;
                        let id = crate::memory::remember(&write, &address, &text, &tags)?;
                        let out = format!("Remembered fact {id}: {text}");
                        Ok(local_tools::ToolResult::basic(
                            "memory_write".into(),
                            ToolCallKind::Edit,
                            serde_json::json!({}),
                            out,
                            true,
                            "Allow durable memory mutation?".into(),
                        ))
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            "kill_task" | "cancel_task" => {
                let id = args
                    .get("id")
                    .or_else(|| args.get("task_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    return Ok("ERROR: kill_task requires id".into());
                }
                // Prefer background task cancel; also try subagent cancel (#179).
                let bg = self.cancel_background_task(&id);
                let sub = self.cancel_subagent(&id);
                match (bg, sub) {
                    (Ok(()), _) => Ok(format!("killed background task {id}")),
                    (_, Ok(())) => Ok(format!("cancelled subagent {id}")),
                    (Err(e1), Err(e2)) => {
                        Ok(format!("ERROR: kill_task {id}: bg={e1}; subagent={e2}"))
                    }
                }
            }
            "task_output" | "get_task_output" => {
                let id = args
                    .get("id")
                    .or_else(|| args.get("task_id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let tasks = self.background_tasks();
                let subs = self.subagents();
                if let Some(id) = id {
                    if let Some(t) = tasks.iter().find(|t| t.id == id) {
                        return Ok(format!(
                            "task {} status={} kind={} detail={}",
                            t.id,
                            t.status,
                            t.kind,
                            t.detail.clone().unwrap_or_default()
                        ));
                    }
                    if let Some(s) = subs.iter().find(|s| s.id == id) {
                        return Ok(format!(
                            "subagent {} kind={} status={} mode={} cwd={} summary={}",
                            s.id,
                            s.kind,
                            s.status,
                            s.execution_mode.as_str(),
                            s.cwd.clone().unwrap_or_default(),
                            s.summary.clone().unwrap_or_default()
                        ));
                    }
                    return Ok(format!("ERROR: unknown task/subagent id {id}"));
                }
                let mut lines = Vec::new();
                for t in &tasks {
                    lines.push(format!("task {} [{}] {}", t.id, t.status, t.title));
                }
                for s in &subs {
                    lines.push(format!(
                        "subagent {} [{}] {} mode={} cwd={}",
                        s.id,
                        s.status,
                        s.title,
                        s.execution_mode.as_str(),
                        s.cwd.clone().unwrap_or_default()
                    ));
                }
                if lines.is_empty() {
                    Ok("(no background tasks or subagents)".into())
                } else {
                    Ok(lines.join("\n"))
                }
            }
            "memory_read" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let address = self.memory_address_from_args(session_id, &args)?;
                let facts = crate::memory::search(&address, &query)?;
                let out = if facts.is_empty() {
                    format!("(no matching {} memory)", address.scope().label())
                } else {
                    facts
                        .iter()
                        .map(|f| format!("- {}", f.text))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let call_id = Uuid::new_v4().to_string();
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "memory_read".into(),
                    kind: ToolCallKind::Read,
                    status: ToolCallStatus::Completed,
                    input: args.clone(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Completed,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "memory_read",
                    ToolCallStatus::Completed,
                    Some(out.clone()),
                );
                Ok(out)
            }
            "web_fetch" => {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("web_fetch requires url"))?
                    .to_string();
                if self.session_sandbox_is_readonly(session_id) {
                    return Ok("ERROR: tool safety profile is read-only; web_fetch denied".into());
                }
                self.run_tool_for_output(
                    session_id,
                    "web_fetch",
                    &args,
                    || {
                        let url = url.clone();
                        async move { tool_web_fetch(&url).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            other => Ok(format!(
                "Unknown tool `{other}`. Available: list_dir, read_file, grep, write_file, write_files, \
                 run_terminal_cmd, glob_files, apply_patch, spawn_explore, spawn_general_purpose, \
                 todo_write, memory_write, memory_read, web_fetch, and mcp__* tools"
            )),
        }
    }

    /// Read-only explore subagent: gather layout/search hits and return a summary.
    async fn run_explore_subagent(
        &self,
        session_id: Uuid,
        cwd: &Path,
        query: &str,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String> {
        let sub_id = Uuid::new_v4().to_string();
        {
            let mut g = self.inner.lock();
            g.subagents.push(SubagentInfo {
                id: sub_id.clone(),
                kind: "explore".into(),
                title: query.chars().take(48).collect(),
                status: "running".into(),
                session_id: Some(session_id.to_string()),
                summary: None,
                last_tool: None,
                cwd: Some(cwd.display().to_string()),
                execution_mode: SubagentExecutionMode::SharedReadOnly,
            });
        }
        let _ = event_tx.send(SessionUpdate::SubagentSpawned {
            session_id,
            subagent_id: sub_id.clone(),
            kind: "explore".into(),
            title: query.chars().take(64).collect(),
        });

        if cancel.is_cancelled() {
            self.finish_subagent(&sub_id, "cancelled", event_tx, session_id, None);
            return Ok("(explore cancelled)".into());
        }

        // Deterministic explore: list + glob + optional grep (read-only tools).
        let listing = local_tools::tool_list_dir(cwd, ".")
            .await
            .map(|t| t.output)
            .unwrap_or_else(|e| format!("list_dir error: {e}"));
        let globs = crate::project_context::glob_files(cwd, "*.{rs,ts,tsx,js,py,md,toml,json}", 40);
        let mut parts = vec![
            format!("## Explore: {query}"),
            "### Project root listing".into(),
            listing.chars().take(4_000).collect(),
            "### Sample files".into(),
            if globs.is_empty() {
                "(no matches)".into()
            } else {
                globs.join("\n")
            },
        ];

        // Keyword grep from query tokens
        let tokens: Vec<&str> = query
            .split_whitespace()
            .filter(|t| {
                if t.len() <= 2 {
                    return false;
                }
                let l = t.to_ascii_lowercase();
                !matches!(l.as_str(), "the" | "and" | "for" | "with" | "this")
            })
            .take(3)
            .collect();
        for tok in tokens {
            if cancel.is_cancelled() {
                break;
            }
            if let Ok(tr) = local_tools::tool_grep(cwd, tok, ".").await {
                parts.push(format!(
                    "### grep `{tok}`\n{}",
                    tr.output.chars().take(2_000).collect::<String>()
                ));
            }
        }

        // Online: optional short model summary of findings
        let mut summary = parts.join("\n\n");
        if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_none() {
            let model = self.inner.lock().model.clone();
            if let Some(creds) = crate::auth_store::resolve_wire_credentials_for_model(&model)
                .map_err(anyhow::Error::msg)?
            {
                let ask = format!(
                    "You are a read-only explore agent. Summarize findings for the parent agent.\n\
                     Query: {query}\n\nFindings:\n{}",
                    summary.chars().take(8_000).collect::<String>()
                );
                let usage_attempt = match self.begin_provider_attempt(session_id).await {
                    Ok(attempt) => attempt,
                    Err(error) => {
                        if let Some(stop) = self.run_token_stop_before_request(session_id) {
                            summary = format!("{summary}\n\n### Explorer summary\n{stop}");
                            None
                        } else {
                            return Err(error);
                        }
                    }
                };
                if usage_attempt.is_some()
                    || self.run_token_stop_before_request(session_id).is_none()
                {
                    match call_xai_chat(
                        &creds,
                        &model,
                        &[("user".into(), ask)],
                        None,
                        cwd,
                        SessionKind::Build,
                    )
                    .await
                    {
                        Ok(reply) => {
                            let stop = self.finish_provider_attempt(
                                session_id,
                                usage_attempt,
                                reply.usage.as_ref(),
                            )?;
                            summary = format!(
                                "{summary}\n\n### Explorer summary\n{}{}",
                                reply.text,
                                stop.map(|stop| format!("\n\n{stop}")).unwrap_or_default()
                            );
                        }
                        Err(_error) => {
                            if let Some(stop) =
                                self.finish_provider_attempt(session_id, usage_attempt, None)?
                            {
                                summary = format!("{summary}\n\n### Explorer summary\n{stop}");
                            } else {
                                summary = format!(
                                    "{summary}\n\n### Explorer summary\n(model summary unavailable)"
                                );
                            }
                        }
                    }
                }
            }
        }

        let clipped: String = summary.chars().take(20_000).collect();
        self.finish_subagent(
            &sub_id,
            "completed",
            event_tx,
            session_id,
            Some(clipped.chars().take(200).collect()),
        );
        Ok(clipped)
    }

    /// Spawn a GP/plan child on a background task and return immediately (#151).
    /// Multiple spawns therefore overlap (true parallelism via JoinHandle tasks).
    fn spawn_gp_subagent_parallel(
        &self,
        session_id: Uuid,
        cwd: &Path,
        prompt: &str,
        kind: &str,
        parent_cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String> {
        let write = self.durable_write("spawning a subagent")?;
        // kind may be `general-purpose`, `plan`, or `kind@persona` (#164).
        let (kind, persona_name) = if let Some((k, p)) = kind.split_once('@') {
            (k.trim(), Some(p.trim()))
        } else {
            (kind.trim(), None)
        };
        let kind = if kind == "plan" {
            "plan"
        } else {
            "general-purpose"
        };
        let persona_layer = {
            let project = self.inner.lock().project_cwd.clone();
            persona_name
                .and_then(|n| crate::agents_personas::resolve_persona(project.as_deref(), n))
        };
        let sub_id = Uuid::new_v4().to_string();
        let kind_label = if let Some(ref p) = persona_layer {
            format!("{}@{}", kind, p.name)
        } else {
            kind.into()
        };
        let configured_isolation = self.inner.lock().subagent_isolation;
        let (isolation_preference, _) = effective_subagent_isolation(configured_isolation);
        let mut child_cwd = cwd.to_path_buf();
        let execution_mode = if kind == "plan" {
            SubagentExecutionMode::SharedReadOnly
        } else {
            match isolation_preference {
                SubagentIsolationPreference::Worktree => {
                    match crate::isolation::prepare_isolation_cwd(cwd, &sub_id) {
                        Ok(isolated_cwd) => {
                            let mode = if isolated_cwd.join(".git").is_file() {
                                SubagentExecutionMode::Worktree
                            } else {
                                SubagentExecutionMode::ProjectCopy
                            };
                            child_cwd = isolated_cwd;
                            mode
                        }
                        Err(error) => {
                            let detail = format!("isolation failed: {error}");
                            {
                                let mut g = self.inner.lock();
                                g.subagents.push(SubagentInfo {
                                    id: sub_id.clone(),
                                    kind: kind_label.clone(),
                                    title: prompt.chars().take(48).collect(),
                                    status: "failed".into(),
                                    session_id: Some(session_id.to_string()),
                                    summary: Some(detail.clone()),
                                    last_tool: None,
                                    cwd: None,
                                    execution_mode: SubagentExecutionMode::IsolationFailed,
                                });
                            }
                            let _ = event_tx.send(SessionUpdate::SubagentSpawned {
                                session_id,
                                subagent_id: sub_id.clone(),
                                kind: kind_label.clone(),
                                title: prompt.chars().take(64).collect(),
                            });
                            let _ = event_tx.send(SessionUpdate::SubagentUpdate {
                                session_id,
                                subagent_id: sub_id.clone(),
                                status: "failed".into(),
                                detail: Some(detail),
                            });
                            let snap = self.inner.lock().subagents.clone();
                            let _ =
                                session_store::save_session_subagents(&write, session_id, &snap);
                            return Ok(format!(
                                "ERROR: subagent isolation failed (not starting child): {error}. \
                                 Choose shared cwd explicitly to permit mutating the parent workspace."
                            ));
                        }
                    }
                }
                SubagentIsolationPreference::Shared => SubagentExecutionMode::SharedMutating,
            }
        };
        // Child is cancelled if parent is cancelled *or* cancel_subagent is called.
        let child_cancel = parent_cancel.child_token();
        {
            let mut g = self.inner.lock();
            g.subagents.push(SubagentInfo {
                id: sub_id.clone(),
                kind: kind_label.clone(),
                title: prompt.chars().take(48).collect(),
                status: "running".into(),
                session_id: Some(session_id.to_string()),
                summary: None,
                last_tool: None,
                cwd: Some(child_cwd.display().to_string()),
                execution_mode,
            });
            g.subagent_cancels
                .insert(sub_id.clone(), child_cancel.clone());
        }
        let _ = event_tx.send(SessionUpdate::SubagentSpawned {
            session_id,
            subagent_id: sub_id.clone(),
            kind: kind_label.clone(),
            title: prompt.chars().take(64).collect(),
        });
        // Persist "running" row so reopen can show in-flight / history (#152).
        {
            let snap = self.inner.lock().subagents.clone();
            let _ = session_store::save_session_subagents(&write, session_id, &snap);
        }

        let host = self.clone();
        let event_tx = event_tx.clone();
        let prompt = prompt.to_string();
        let kind_owned = kind.to_string();
        let persona_reminder = persona_layer
            .as_ref()
            .map(crate::agents_personas::persona_system_reminder);
        // Snapshot the durable parent Run identity. Looking this up again from
        // the session after the parent finishes could charge a later Run.
        let run_usage_tracker = self.run_usage_trackers.lock().get(&session_id).cloned();
        let sub_id_task = sub_id.clone();
        // Subagents capture a host clone, so ordered shutdown must cancel and
        // join them before the process lock is released (#455).
        let subagent_cancel = child_cancel.clone();
        let shutdown = self.shutdown_token();
        let _ = self.spawn_supervised("cascading shutdown to a subagent", async move {
            shutdown.cancelled().await;
            subagent_cancel.cancel();
        });
        let spawned = self.spawn_supervised("spawning a subagent", async move {
            host.run_gp_subagent_body(
                session_id,
                &child_cwd,
                &prompt,
                &kind_owned,
                &sub_id_task,
                child_cancel,
                event_tx,
                persona_reminder,
                run_usage_tracker,
            )
            .await;
        });
        if spawned.is_err() {
            let mut g = self.inner.lock();
            g.subagent_cancels.remove(&sub_id);
            if let Some(entry) = g.subagents.iter_mut().find(|entry| entry.id == sub_id) {
                entry.status = "cancelled".into();
                entry.summary = Some("host shutdown".into());
            }
        }

        let isolation_note = match execution_mode {
            SubagentExecutionMode::Worktree => "isolated worktree",
            SubagentExecutionMode::ProjectCopy => "isolated project copy",
            SubagentExecutionMode::SharedReadOnly => "shared read-only cwd",
            SubagentExecutionMode::SharedMutating => "shared mutating cwd (explicit user opt-in)",
            SubagentExecutionMode::IsolationFailed | SubagentExecutionMode::Unknown => {
                "unknown cwd mode"
            }
        };
        Ok(format!(
            "Spawned {kind_label} subagent `{sub_id}` in {isolation_note} \
             (running in parallel — parent is not blocked)."
        ))
    }

    /// Body of a GP child (runs on a JoinHandle task).
    #[allow(clippy::too_many_arguments)]
    async fn run_gp_subagent_body(
        &self,
        session_id: Uuid,
        cwd: &Path,
        prompt: &str,
        kind: &str,
        sub_id: &str,
        cancel: CancellationToken,
        event_tx: crate::event_bus::EventBus,
        persona_reminder: Option<String>,
        run_usage_tracker: Option<Arc<RunUsageTracker>>,
    ) {
        if cancel.is_cancelled() {
            self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
            return;
        }

        // Offline deterministic GP: optional sleep for parallel tests + write.
        if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
            let mut parts = vec![format!("## GP subagent ({kind}): {prompt}")];
            if let Some(ref pr) = persona_reminder {
                parts.push(pr.clone());
            }
            // Parallel test hook: "sleep_ms:N ..." delays without blocking parent.
            if let Some(rest) = prompt.strip_prefix("sleep_ms:") {
                let ms: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(50);
                let sleep = tokio::time::sleep(std::time::Duration::from_millis(ms));
                tokio::pin!(sleep);
                tokio::select! {
                    _ = cancel.cancelled() => {
                        self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
                        return;
                    }
                    _ = &mut sleep => {}
                }
                parts.push(format!("### slept {ms}ms"));
            }
            if let Ok(tr) = local_tools::tool_list_dir(cwd, ".").await {
                parts.push(format!(
                    "### listing\n{}",
                    tr.output.chars().take(2_000).collect::<String>()
                ));
            }
            if let Some(rest) = prompt.find("write ").map(|i| &prompt[i + "write ".len()..]) {
                // #161: plan capability blocks mutators offline the same as online.
                if kind == "plan" || kind == "explore" {
                    parts.push(format!(
                        "### write DENIED by capability mode `{kind}`: \
                         write_file is not allowed for plan/explore children"
                    ));
                } else if let Some((path, content)) = rest.split_once(':') {
                    self.snapshot_edit_original_for_session(session_id, cwd, path.trim());
                    if let Ok(tr) =
                        local_tools::tool_write_file(cwd, path.trim(), content.trim()).await
                    {
                        parts.push(format!("### write\n{}", tr.output));
                        {
                            let mut g = self.inner.lock();
                            if let Some(s) = g.subagents.iter_mut().find(|s| s.id == sub_id) {
                                s.last_tool = Some("write_file".into());
                            }
                        }
                        self.emit_file_edit(session_id, cwd, path.trim(), &tr.output, &event_tx);
                    }
                }
            }
            if cancel.is_cancelled() {
                self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
                return;
            }
            let summary = parts.join("\n\n");
            let clipped: String = summary.chars().take(12_000).collect();
            self.finish_subagent(
                sub_id,
                "completed",
                &event_tx,
                session_id,
                Some(clipped.chars().take(400).collect()),
            );
            return;
        }

        // Online: short multi-tool agent loop under child cancel.
        let creds = match crate::auth_store::resolve_wire_credentials_for_model(
            &self.inner.lock().model.clone(),
        ) {
            Err(error) => {
                self.finish_subagent(sub_id, "failed", &event_tx, session_id, Some(error));
                return;
            }
            Ok(None) => {
                let msg = "GP subagent: no credentials";
                self.finish_subagent(sub_id, "failed", &event_tx, session_id, Some(msg.into()));
                return;
            }
            Ok(Some(c)) => c,
        };
        let model = self.inner.lock().model.clone();
        let effort = self.inner.lock().effort;
        let (tools, mcp_index) = coding_agent_tools(&[]);
        let mut sys = format!(
            "You are a {kind} subagent for GrokPtah. Complete the task with tools. \
             Return a concise summary for the parent when done."
        );
        if let Some(ref pr) = persona_reminder {
            sys.push('\n');
            sys.push_str(pr);
        }
        let mut messages = vec![
            serde_json::json!({
                "role": "system",
                "content": sys
            }),
            serde_json::json!({ "role": "user", "content": prompt }),
        ];
        let mut last = String::new();
        // #163: deeper child sessions (default 16 rounds; was hard-capped at 6).
        let max_child_rounds: u32 = std::env::var("GROKPTAH_SUBAGENT_MAX_ROUNDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16)
            .clamp(4, 48);
        for _round in 1..=max_child_rounds {
            if cancel.is_cancelled() {
                self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
                return;
            }
            if let Some(stop) = run_usage_tracker
                .as_ref()
                .and_then(|tracker| tracker.stop_message())
            {
                last = stop;
                break;
            }
            let usage_attempt =
                match Self::begin_provider_attempt_for_tracker(run_usage_tracker.clone()).await {
                    Ok(attempt) => attempt,
                    Err(error) => {
                        last = run_usage_tracker
                            .as_ref()
                            .and_then(|tracker| tracker.stop_message())
                            .unwrap_or_else(|| format!("GP subagent admission failed: {error:#}"));
                        break;
                    }
                };
            let provider_observation = self.provider_observation_context(session_id);
            let step = call_xai_agent_step_observed(
                &creds,
                &model,
                effort,
                &messages,
                &tools,
                !run_usage_tracker
                    .as_ref()
                    .is_some_and(|tracker| tracker.is_bounded()),
                &cancel,
                provider_observation.as_ref(),
                |_d| {},
                |_t| {},
            )
            .await;
            let step = match step {
                Ok(step) => step,
                Err(error) => {
                    match self.finish_provider_attempt(session_id, usage_attempt, None) {
                        Ok(Some(stop)) => last = stop,
                        Ok(None) => last = format!("GP subagent model call failed: {error:#}"),
                        Err(persist_error) => {
                            last =
                                format!("GP subagent usage persistence failed: {persist_error:#}")
                        }
                    }
                    break;
                }
            };
            let token_stop = match self.finish_provider_attempt(
                session_id,
                usage_attempt,
                match &step {
                    AgentStep::Final { usage, .. } | AgentStep::ToolCalls { usage, .. } => {
                        usage.as_ref()
                    }
                },
            ) {
                Ok(stop) => stop,
                Err(error) => {
                    last = format!("GP subagent usage persistence failed: {error:#}");
                    break;
                }
            };
            match step {
                AgentStep::Final {
                    text, reasoning, ..
                } => {
                    if let Some(r) = reasoning {
                        push_thought(self, session_id, &r);
                    }
                    last = match token_stop {
                        Some(stop) if text.trim().is_empty() => stop,
                        Some(stop) => format!("{text}\n\n{stop}"),
                        None => text,
                    };
                    break;
                }
                AgentStep::ToolCalls {
                    content,
                    tool_calls,
                    reasoning,
                    ..
                } => {
                    if let Some(r) = reasoning {
                        push_thought(self, session_id, &r);
                    }
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls.iter().map(|tc| serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": { "name": tc.name, "arguments": tc.arguments }
                        })).collect::<Vec<_>>(),
                    }));
                    for tc in tool_calls {
                        if cancel.is_cancelled() {
                            break;
                        }
                        {
                            let mut g = self.inner.lock();
                            if let Some(s) = g.subagents.iter_mut().find(|s| s.id == sub_id) {
                                s.last_tool = Some(tc.name.clone());
                            }
                        }
                        // #161 capability modes: plan is non-mutating (explore is separate path).
                        let out = if tc.name.starts_with("spawn_") {
                            format!("DENIED: nested {} not allowed inside subagent", tc.name)
                        } else if kind == "plan" && plan_subagent_denies_tool(&tc.name) {
                            format!(
                                "DENIED by capability mode `plan`: tool `{}` is not allowed. \
                                 Plan agents may only research (list/read/grep/glob) and produce a plan.",
                                tc.name
                            )
                        } else {
                            Box::pin(self.dispatch_agent_tool(
                                session_id,
                                cwd,
                                &tc.name,
                                &tc.arguments,
                                &cancel,
                                &event_tx,
                                &mcp_index,
                            ))
                            .await
                            .unwrap_or_else(|e| format!("ERROR: {e}"))
                        };
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": out.chars().take(8_000).collect::<String>(),
                        }));
                        last = out;
                    }
                    if let Some(stop) = token_stop {
                        last = stop;
                        break;
                    }
                }
            }
        }
        let clipped: String = last.chars().take(12_000).collect();
        self.finish_subagent(
            sub_id,
            if cancel.is_cancelled() {
                "cancelled"
            } else {
                "completed"
            },
            &event_tx,
            session_id,
            Some(clipped.chars().take(400).collect()),
        );
    }

    fn finish_subagent(
        &self,
        sub_id: &str,
        status: &str,
        event_tx: &crate::event_bus::EventBus,
        session_id: Uuid,
        detail: Option<String>,
    ) {
        let write = self.durable_write("recording a subagent outcome");
        let snap = {
            let mut g = self.inner.lock();
            g.subagent_cancels.remove(sub_id);
            if let Some(s) = g.subagents.iter_mut().find(|s| s.id == sub_id) {
                s.status = status.into();
                if let Some(ref d) = detail {
                    s.summary = Some(d.clone());
                }
            }
            g.subagents.clone()
        };
        match &write {
            Ok(write) => {
                let _ = session_store::save_session_subagents(write, session_id, &snap);
            }
            Err(error) => {
                eprintln!("[grokptah] subagent outcome not persisted: {error:#}");
            }
        }
        let _ = event_tx.send(SessionUpdate::SubagentUpdate {
            session_id,
            subagent_id: sub_id.to_string(),
            status: status.into(),
            detail,
        });
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_mcp_tool(
        &self,
        session_id: Uuid,
        cwd: &Path,
        server: &str,
        tool: &str,
        wire_name: &str,
        args: &serde_json::Value,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }
        match self.session_agent_authority(session_id) {
            Ok(Some(policy))
                if !policy
                    .allowed_mcp_servers
                    .iter()
                    .any(|allowed| allowed == "*" || allowed == server) =>
            {
                return Ok(format!(
                    "DENIED by Agent capability policy: MCP server `{server}`"
                ));
            }
            Err(_) => {
                return Ok("DENIED: persistent Agent policy is unavailable".into());
            }
            _ => {}
        }
        let gate = self.tool_gate_inner(session_id, wire_name, false);
        if gate == ToolGate::AutoDeny {
            return Ok(format!("DENIED by deny rule: MCP `{wire_name}`"));
        }
        let always = matches!(gate, ToolGate::AutoAllow);
        let call_id = Uuid::new_v4().to_string();
        if !always {
            let decision = self
                .prompt_tool_permission(
                    session_id,
                    wire_name,
                    format!("Allow MCP tool `{server}/{tool}`?"),
                    args.clone(),
                    cancel,
                )
                .await;
            if decision == PermissionDecision::Deny {
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: wire_name.into(),
                    kind: ToolCallKind::Other,
                    status: ToolCallStatus::Denied,
                    input: args.clone(),
                });
                return Ok(format!("DENIED: user denied MCP tool `{server}/{tool}`"));
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert(wire_name.into());
            }
        }
        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: wire_name.into(),
            kind: ToolCallKind::Other,
            status: ToolCallStatus::Running,
            input: args.clone(),
        });
        let result = tokio::select! {
            r = crate::mcp_runtime::call_mcp_tool(Some(cwd), server, tool, args.clone()) => r,
            _ = cancel.cancelled() => {
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some("(cancelled)".into()),
                });
                return Ok("(cancelled)".into());
            }
        };
        match result {
            Ok(out) => {
                let clipped = if out.len() > 24_000 {
                    crate::textutil::truncate_with_marker(&out, 24_000, "…\n(truncated)")
                } else {
                    out
                };
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Completed,
                    output: Some(clipped.clone()),
                });
                Ok(clipped)
            }
            Err(e) => {
                let msg = format!("MCP error ({server}/{tool}): {e:#}");
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
                Ok(msg)
            }
        }
    }

    /// Like run_tool_call but returns the tool output string (or denial).
    async fn run_tool_for_output<F, Fut>(
        &self,
        session_id: Uuid,
        tool_name: &str,
        input: &serde_json::Value,
        f: F,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<local_tools::ToolResult>>,
    {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }

        // Tool safety profile: deny writes in read-only for shared tool path.
        if matches!(
            tool_name,
            "write_file" | "write_files" | "apply_patch" | "memory_write"
        ) && self.session_sandbox_is_readonly(session_id)
        {
            return Ok(format!(
                "ERROR: tool safety profile is read-only; {tool_name} denied"
            ));
        }

        // PreToolUse hooks can deny before permission UI / execution.
        let session_workspace = {
            let g = self.inner.lock();
            g.sessions
                .get(&session_id)
                .map(|session| session.cwd.clone())
                .ok_or_else(|| anyhow!("unknown session"))?
        };
        if let Some(msg) =
            crate::hooks::pre_tool_use_deny(Some(&session_workspace), tool_name, input)
        {
            let call_id = Uuid::new_v4().to_string();
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: tool_name.into(),
                kind: tool_kind(tool_name),
                status: ToolCallStatus::Denied,
                input: input.clone(),
            });
            let out = format!("DENIED by hook: {msg}");
            let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                session_id,
                call_id,
                status: ToolCallStatus::Denied,
                output: Some(out.clone()),
            });
            return Ok(out);
        }

        let call_id = Uuid::new_v4().to_string();
        let needs_perm = matches!(
            tool_name,
            "run_terminal_cmd" | "write_file" | "write_files" | "apply_patch" | "memory_write"
        );
        let gate = self.tool_gate(session_id, tool_name);
        if gate == ToolGate::AutoDeny {
            return Ok(format!("DENIED by deny rule: tool `{tool_name}`"));
        }
        let always = matches!(gate, ToolGate::AutoAllow);

        if needs_perm && !always {
            let decision = self
                .prompt_tool_permission(
                    session_id,
                    tool_name,
                    format!("Allow tool `{tool_name}`?"),
                    input.clone(),
                    cancel,
                )
                .await;
            if decision == PermissionDecision::Deny {
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: tool_name.into(),
                    kind: tool_kind(tool_name),
                    status: ToolCallStatus::Denied,
                    input: input.clone(),
                });
                return Ok(format!("DENIED: user denied tool `{tool_name}`"));
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert(tool_name.into());
            }
        }

        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: tool_name.into(),
            kind: tool_kind(tool_name),
            status: ToolCallStatus::Running,
            input: input.clone(),
        });
        push_tool(
            self,
            session_id,
            &call_id,
            tool_name,
            ToolCallStatus::Running,
            None,
        );

        match f().await {
            Ok(tr) => {
                let out = tr.output.clone();
                let status = if tr.cancelled {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };
                let status_s = if tr.cancelled { "failed" } else { "completed" };
                let _ = crate::hooks::post_tool_use_note(
                    Some(&session_workspace),
                    tool_name,
                    status_s,
                    &out,
                );
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    tool_name,
                    status,
                    Some(out.clone()),
                );
                Ok(out)
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = crate::hooks::post_tool_use_note(
                    Some(&session_workspace),
                    tool_name,
                    "failed",
                    &msg,
                );
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    tool_name,
                    ToolCallStatus::Failed,
                    Some(msg.clone()),
                );
                Ok(format!("ERROR: {msg}"))
            }
        }
    }

    async fn run_shell_tool_for_output(
        &self,
        session_id: Uuid,
        cwd: &Path,
        command: &str,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }
        let call_id = Uuid::new_v4().to_string();

        // #155 exec-risk preflight (not OS sandbox)
        let risk = crate::exec_risk::assess_shell_risk(command);
        let (sandbox_profile, yolo) = self.session_exec_risk_policy(session_id);
        if risk.tier == crate::exec_risk::RiskTier::Deny
            && crate::exec_risk::should_block_deny_tier(&sandbox_profile, yolo)
        {
            let msg = format!(
                "DENIED by exec-risk: {} (peeled: `{}`). \
                 This is a tool-safety risk gate, not an OS sandbox. \
                 Adjust the command or use a full-profile YOLO session if intentional.",
                risk.reason, risk.peeled
            );
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: "run_terminal_cmd".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({
                    "command": command,
                    "risk": risk.reason,
                    "risk_tier": "deny",
                }),
            });
            push_tool(
                self,
                session_id,
                &call_id,
                "run_terminal_cmd",
                ToolCallStatus::Denied,
                Some(msg.clone()),
            );
            // #156: model-visible deny reason (tool result string)
            return Ok(msg);
        }

        let gate = self.tool_gate(session_id, "run_terminal_cmd");
        if gate == ToolGate::AutoDeny {
            // #156: feed clear reason to the model
            let msg = format!(
                "DENIED by deny rule: shell `{command}` was blocked by permission deny rules. \
                 Do not retry the same command; choose a safer alternative or ask the user."
            );
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: "run_terminal_cmd".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({ "command": command }),
            });
            push_tool(
                self,
                session_id,
                &call_id,
                "run_terminal_cmd",
                ToolCallStatus::Denied,
                Some(msg.clone()),
            );
            return Ok(msg);
        }
        let always = matches!(gate, ToolGate::AutoAllow);

        // Ask-tier risk forces a prompt even under allow-rules (unless YOLO).
        let force_ask = risk.tier == crate::exec_risk::RiskTier::Ask && !yolo;
        if !always || force_ask {
            let risk_note = if risk.tier != crate::exec_risk::RiskTier::Allow {
                format!(" [risk: {}]", risk.reason)
            } else {
                String::new()
            };
            let decision = self
                .prompt_tool_permission(
                    session_id,
                    "run_terminal_cmd",
                    format!("Allow shell: {command}{risk_note}"),
                    serde_json::json!({
                        "tool": "run_terminal_cmd",
                        "command": command,
                        "risk": risk.reason,
                        "risk_tier": match risk.tier {
                            crate::exec_risk::RiskTier::Allow => "allow",
                            crate::exec_risk::RiskTier::Ask => "ask",
                            crate::exec_risk::RiskTier::Deny => "deny",
                        },
                        "peeled": risk.peeled,
                    }),
                    cancel,
                )
                .await;
            if decision == PermissionDecision::Deny {
                let msg = format!(
                    "DENIED: user denied shell `{command}` (reason for model: do not retry; \
                     pick another approach). risk={}",
                    risk.reason
                );
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "run_terminal_cmd".into(),
                    kind: ToolCallKind::Execute,
                    status: ToolCallStatus::Denied,
                    input: serde_json::json!({ "command": command, "risk": risk.reason }),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "run_terminal_cmd",
                    ToolCallStatus::Denied,
                    Some(msg.clone()),
                );
                return Ok(msg);
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert("run_terminal_cmd".into());
            }
        }

        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: "run_terminal_cmd".into(),
            kind: ToolCallKind::Execute,
            status: ToolCallStatus::Running,
            input: serde_json::json!({ "command": command }),
        });
        push_tool(
            self,
            session_id,
            &call_id,
            "run_terminal_cmd",
            ToolCallStatus::Running,
            None,
        );
        let _ = event_tx.send(SessionUpdate::ShellSessionStarted {
            session_id,
            call_id: call_id.clone(),
            command: command.to_string(),
        });
        self.register_shell_background_task(&call_id, command, Some(session_id));

        let live_shells = self.inner.lock().live_shells.clone();
        let event_tx_chunks = event_tx.clone();
        let call_id_chunks = call_id.clone();
        let result = local_tools::tool_shell_streaming(
            cwd,
            command,
            cancel.clone(),
            session_id,
            live_shells,
            move |chunk| {
                let _ = event_tx_chunks.send(SessionUpdate::ShellOutput {
                    session_id,
                    call_id: call_id_chunks.clone(),
                    data: chunk,
                });
            },
        )
        .await;

        match result {
            Ok(tr) => {
                let cancelled = tr.cancelled;
                let exit_code = tr.exit_code;
                let out = tr.output.clone();
                let status = if cancelled || exit_code != Some(0) {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };
                self.complete_shell_background_task(
                    &call_id,
                    if cancelled {
                        "cancelled"
                    } else if exit_code == Some(0) {
                        "completed"
                    } else {
                        "failed"
                    },
                );
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code,
                    cancelled,
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "run_terminal_cmd",
                    status,
                    Some(out.clone()),
                );
                Ok(out)
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code: None,
                    cancelled: cancel.is_cancelled(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "run_terminal_cmd",
                    ToolCallStatus::Failed,
                    Some(msg.clone()),
                );
                Ok(format!("ERROR: {msg}"))
            }
        }
    }

    /// Legacy shell helper (unused by agent loop; kept for call sites).
    #[allow(dead_code)]
    async fn run_shell_tool(
        &self,
        session_id: Uuid,
        cwd: &Path,
        command: &str,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<()> {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let call_id = Uuid::new_v4().to_string();
        let risk = crate::exec_risk::assess_shell_risk(command);
        let (sandbox_profile, yolo) = self.session_exec_risk_policy(session_id);
        if risk.tier == crate::exec_risk::RiskTier::Deny
            && crate::exec_risk::should_block_deny_tier(&sandbox_profile, yolo)
        {
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: "run_terminal_cmd".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({
                    "command": command,
                    "risk": risk.reason,
                    "risk_tier": "deny",
                }),
            });
            return Ok(());
        }
        let needs_perm = true;
        let gate = self.tool_gate(session_id, "run_terminal_cmd");
        if gate == ToolGate::AutoDeny {
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: "run_terminal_cmd".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({ "command": command }),
            });
            return Ok(());
        }
        let always = matches!(gate, ToolGate::AutoAllow);
        let force_ask = risk.tier == crate::exec_risk::RiskTier::Ask && !yolo;

        if (needs_perm && !always) || force_ask {
            let risk_note = if risk.tier != crate::exec_risk::RiskTier::Allow {
                format!(" [risk: {}]", risk.reason)
            } else {
                String::new()
            };
            let decision = self
                .prompt_tool_permission(
                    session_id,
                    "run_terminal_cmd",
                    format!("Allow tool `run_terminal_cmd`?{risk_note}"),
                    serde_json::json!({
                        "tool": "run_terminal_cmd",
                        "command": command,
                        "risk": risk.reason,
                        "risk_tier": match risk.tier {
                            crate::exec_risk::RiskTier::Allow => "allow",
                            crate::exec_risk::RiskTier::Ask => "ask",
                            crate::exec_risk::RiskTier::Deny => "deny",
                        },
                    }),
                    cancel,
                )
                .await;
            if decision == PermissionDecision::Deny {
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "run_terminal_cmd".into(),
                    kind: ToolCallKind::Execute,
                    status: ToolCallStatus::Denied,
                    input: serde_json::json!({ "command": command }),
                });
                return Ok(());
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert("run_terminal_cmd".into());
            }
        }

        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: "run_terminal_cmd".into(),
            kind: ToolCallKind::Execute,
            status: ToolCallStatus::Running,
            input: serde_json::json!({ "command": command }),
        });
        // UI attaches to THIS stream — do not re-run the command in another PTY.
        let _ = event_tx.send(SessionUpdate::ShellSessionStarted {
            session_id,
            call_id: call_id.clone(),
            command: command.to_string(),
        });
        self.register_shell_background_task(&call_id, command, Some(session_id));

        let live_shells = self.inner.lock().live_shells.clone();
        let event_tx_chunks = event_tx.clone();
        let call_id_chunks = call_id.clone();
        let result = local_tools::tool_shell_streaming(
            cwd,
            command,
            cancel.clone(),
            session_id,
            live_shells,
            move |chunk| {
                let _ = event_tx_chunks.send(SessionUpdate::ShellOutput {
                    session_id,
                    call_id: call_id_chunks.clone(),
                    data: chunk,
                });
            },
        )
        .await;

        match result {
            Ok(tr) => {
                let exit_code = tr.exit_code;
                let status = if tr.cancelled || exit_code != Some(0) {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };
                self.complete_shell_background_task(
                    &call_id,
                    if tr.cancelled {
                        "cancelled"
                    } else if exit_code == Some(0) {
                        "completed"
                    } else {
                        "failed"
                    },
                );
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code,
                    cancelled: tr.cancelled,
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status,
                    output: Some(tr.output),
                });
            }
            Err(e) => {
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code: None,
                    cancelled: cancel.is_cancelled(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(e.to_string()),
                });
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    async fn run_tool_call<F, Fut>(
        &self,
        session_id: Uuid,
        tool_name: &str,
        f: F,
        cancel: &CancellationToken,
        event_tx: &crate::event_bus::EventBus,
    ) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<local_tools::ToolResult>>,
    {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let call_id = Uuid::new_v4().to_string();
        let needs_perm = matches!(tool_name, "run_terminal_cmd" | "write_file" | "write_files");
        let gate = self.tool_gate(session_id, tool_name);
        if gate == ToolGate::AutoDeny {
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: tool_name.into(),
                kind: ToolCallKind::Other,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({ "tool": tool_name }),
            });
            return Ok(());
        }
        let always = matches!(gate, ToolGate::AutoAllow);

        if needs_perm && !always {
            let decision = self
                .prompt_tool_permission(
                    session_id,
                    tool_name,
                    format!("Allow tool `{tool_name}`?"),
                    serde_json::json!({ "tool": tool_name }),
                    cancel,
                )
                .await;
            if decision == PermissionDecision::Deny {
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: tool_name.into(),
                    kind: ToolCallKind::Other,
                    status: ToolCallStatus::Denied,
                    input: serde_json::json!({}),
                });
                return Ok(());
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert(tool_name.into());
            }
        }

        let kind = match tool_name {
            "read_file" | "list_dir" => ToolCallKind::Read,
            "write_file" | "write_files" => ToolCallKind::Edit,
            "grep" => ToolCallKind::Search,
            "run_terminal_cmd" => ToolCallKind::Execute,
            _ => ToolCallKind::Other,
        };

        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: tool_name.into(),
            kind,
            status: ToolCallStatus::Running,
            input: serde_json::json!({ "tool": tool_name }),
        });

        match f().await {
            Ok(tr) => {
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Completed,
                    output: Some(tr.output),
                });
            }
            Err(e) => {
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(e.to_string()),
                });
            }
        }
        Ok(())
    }
}

fn effective_subagent_isolation(
    configured: SubagentIsolationPreference,
) -> (SubagentIsolationPreference, bool) {
    let override_mode = std::env::var("GROKPTAH_SUBAGENT_ISOLATION")
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "worktree" | "1" => Some(SubagentIsolationPreference::Worktree),
            // Shared mutation is intentionally accepted only as an explicit value.
            "shared" => Some(SubagentIsolationPreference::Shared),
            _ => None,
        });
    match override_mode {
        Some(mode) => (mode, true),
        None => (configured, false),
    }
}

fn plan_subagent_denies_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "write_file" | "write_files" | "apply_patch" | "run_terminal_cmd" | "memory_write"
    )
}

#[cfg(test)]
mod computer_agent_host_tests {
    use super::*;

    #[test]
    fn plan_subagent_cannot_mutate_durable_memory() {
        assert!(plan_subagent_denies_tool("memory_write"));
        assert!(!plan_subagent_denies_tool("memory_read"));
    }

    #[test]
    fn ephemeral_computer_authority_is_session_scoped_and_model_changes_revoke_it() {
        let _serial = crate::home_override_serial();
        let home = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(home.path().to_path_buf()));

        let host =
            AgentHost::create(HostConfig::default()).expect("acquire the GrokPtah instance lock");
        host.start().unwrap();
        let first = host.session_new().unwrap();
        let second = host.session_new().unwrap();
        let model = host.inner.lock().model.clone();
        host.inner.lock().computer_agent_qualifications.insert(
            (first.id, model.clone()),
            SessionComputerQualification {
                route_fingerprint: "route-a".into(),
            },
        );

        {
            let inner = host.inner.lock();
            assert!(inner
                .computer_agent_qualifications
                .contains_key(&(first.id, model.clone())));
            assert!(!inner
                .computer_agent_qualifications
                .contains_key(&(second.id, model.clone())));
        }

        let (_operation_id, token, guard) = host
            .begin_computer_agent_operation(first.id)
            .expect("first request should reserve the session");
        assert!(host.begin_computer_agent_operation(first.id).is_err());
        host.set_model("different-model".into());
        assert!(token.is_cancelled());
        assert!(host.inner.lock().computer_agent_qualifications.is_empty());
        drop(guard);
        let (_operation_id, restarted_token, _guard) = host
            .begin_computer_agent_operation(first.id)
            .expect("model change should release the old reservation");
        host.stop().unwrap();
        assert!(restarted_token.is_cancelled());

        drop(host);
        crate::set_grokptah_home_override(None);
    }

    #[test]
    fn model_projection_uses_live_credential_route_not_cached_auth_state() {
        let _serial = crate::home_override_serial();
        let home = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let previous_base = std::env::var_os("XAI_API_BASE");
        let previous_key = std::env::var_os("XAI_API_KEY");
        unsafe {
            std::env::set_var("XAI_API_BASE", "https://projection-route.example/v1");
            std::env::set_var("XAI_API_KEY", "live-api-key");
        }

        let catalog = crate::models_catalog::lookup("grok-4.5").unwrap();
        let measured_model = |tools: bool| crate::gateway_config::ProviderModel {
            id: "grok-4.5".into(),
            display_name: "Grok 4.5".into(),
            wire_model_id: Some(catalog.wire_model.clone()),
            capabilities: crate::gateway_config::ModelCapabilities {
                chat: tools,
                tools,
                stream: tools,
                source: crate::gateway_config::CapabilitySource::Measured,
                qualification_schema: Some(
                    crate::gateway_config::CAPABILITY_QUALIFICATION_SCHEMA.into(),
                ),
                ..crate::gateway_config::ModelCapabilities::default()
            },
        };
        let managed_profile =
            |credential_ref: &str, model| crate::gateway_config::ProviderProfile {
                id: crate::gateway_config::XAI_PROVIDER_ID.into(),
                label: "xAI".into(),
                kind: crate::gateway_config::ProviderKind::Xai,
                dialect: crate::gateway_config::ProviderDialect::XaiChatCompletions,
                deadline_class: crate::gateway_config::ProviderDeadlineClass::Standard,
                base_url: "https://projection-route.example/v1".into(),
                credential_ref: Some(credential_ref.into()),
                models: vec![model],
                managed_by_env: false,
                managed_by_host: true,
            };
        let api_model = measured_model(false);
        let api_profile = managed_profile("managed:xai:api-key", api_model.clone());
        let live_credentials = crate::auth_store::resolve_wire_credentials_for_model("grok-4.5")
            .unwrap()
            .unwrap();
        let live_fingerprint = live_credentials.qualification_identity_fingerprint();
        crate::gateway_config::save_managed_profile_capabilities(
            &crate::host_runtime::DurableWriteGuard::unowned_for_test(),
            &api_profile,
            &api_model,
            &live_fingerprint,
        )
        .unwrap();
        let oidc_model = measured_model(true);
        let oidc_profile = managed_profile("managed:xai:oidc", oidc_model.clone());
        crate::gateway_config::save_managed_profile_capabilities(
            &crate::host_runtime::DurableWriteGuard::unowned_for_test(),
            &oidc_profile,
            &oidc_model,
            "v1-sha256:other-oidc-principal",
        )
        .unwrap();

        let host =
            AgentHost::create(HostConfig::default()).expect("acquire the GrokPtah instance lock");
        host.inner.lock().auth = AuthState {
            signed_in: true,
            display_name: Some("stale Grok Build session".into()),
            method: Some("grok_build:oidc".into()),
        };
        let projected = host
            .models()
            .into_iter()
            .find(|model| model.id == "grok-4.5")
            .unwrap();
        assert_eq!(projected.capability_source, "measured");
        assert!(!projected.supports_tools);
        assert!(!projected.supports_stream);

        unsafe {
            if let Some(value) = previous_base {
                std::env::set_var("XAI_API_BASE", value);
            } else {
                std::env::remove_var("XAI_API_BASE");
            }
            if let Some(value) = previous_key {
                std::env::set_var("XAI_API_KEY", value);
            } else {
                std::env::remove_var("XAI_API_KEY");
            }
        }
        drop(host);
        crate::set_grokptah_home_override(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};
    use crate::event_bus::EventReceiver;
    use crate::orchestration::WorkPolicy;

    struct TestHome {
        _tmp: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            set_grokptah_home_override(None);
        }
    }

    struct TestEnvOverride {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl TestEnvOverride {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: tests that mutate process-wide host configuration hold
            // `home_override_serial`, and CI runs this suite with one thread.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for TestEnvOverride {
        fn drop(&mut self) {
            // SAFETY: see `TestEnvOverride::set`; this restores the exact
            // process state observed before the scoped override.
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn test_host() -> (TestHome, HostRuntime, Uuid) {
        let lock = home_override_serial();
        let tmp = tempfile::tempdir().expect("test home");
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).expect("sessions directory");
        set_grokptah_home_override(Some(home));

        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        })
        .expect("acquire the GrokPtah instance lock");
        host.start().expect("start host");
        let session = host
            .session_new_kind(SessionKind::Build)
            .expect("create build session");
        (
            TestHome {
                _tmp: tmp,
                _lock: lock,
            },
            host,
            session.id,
        )
    }

    fn usage_test_run(run_id: &str, max_total_tokens: Option<u64>) -> RunRecord {
        let now = Utc::now();
        RunRecord {
            run_id: run_id.into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/project".into(),
            request_id: format!("request-{run_id}"),
            client_id: Some("test".into()),
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
            bounds: RunBounds {
                max_total_tokens,
                ..RunBounds::default()
            },
            prompt_preview: "test".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        }
    }

    #[test]
    fn run_usage_tracker_persists_cumulative_usage_and_typed_ceiling_stop() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("orch");
        let store = OrchStore::open(&path).unwrap();
        let run = usage_test_run("usage-ceiling", Some(10));
        store.save_run(&run).unwrap();
        let tracker = RunUsageTracker::from_run(store.clone(), &run);

        assert!(tracker
            .record(Some(&CompletionUsage {
                prompt_tokens: 3,
                completion_tokens: 1,
                total_tokens: 4,
                requests: 1,
            }))
            .unwrap()
            .is_none());
        let stop = tracker
            .record(Some(&CompletionUsage {
                prompt_tokens: 4,
                completion_tokens: 2,
                total_tokens: 6,
                requests: 1,
            }))
            .unwrap()
            .unwrap();
        assert!(stop.contains("max_total_tokens ceiling of 10"));
        assert_eq!(tracker.stop_code(), Some("max_total_tokens_reached"));

        let persisted = store.load_run("usage-ceiling").unwrap().unwrap();
        assert_eq!(persisted.aggregates.usage.total_tokens, 10);
        assert_eq!(persisted.aggregates.usage.requests, 2);
        assert!(persisted.aggregates.usage_complete);
        assert_eq!(
            persisted.error_code.as_deref(),
            Some("max_total_tokens_reached")
        );
        assert_eq!(persisted.stop_cause, Some(RunStopCause::TokenCeiling));

        drop(tracker);
        drop(store);
        let reopened = OrchStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .load_run("usage-ceiling")
                .unwrap()
                .unwrap()
                .aggregates
                .usage
                .total_tokens,
            10
        );
    }

    #[test]
    fn bounded_missing_usage_fails_closed_while_unbounded_run_stays_observable() {
        let temp = tempfile::tempdir().unwrap();
        let store = OrchStore::open(temp.path().join("orch")).unwrap();
        let bounded = usage_test_run("bounded-missing", Some(100));
        let unbounded = usage_test_run("unbounded-missing", None);
        store.save_run(&bounded).unwrap();
        store.save_run(&unbounded).unwrap();

        let bounded_tracker = RunUsageTracker::from_run(store.clone(), &bounded);
        assert!(bounded_tracker.record(None).unwrap().is_some());
        assert_eq!(
            bounded_tracker.stop_code(),
            Some("max_total_tokens_usage_unavailable")
        );
        assert_eq!(
            store
                .load_run("bounded-missing")
                .unwrap()
                .unwrap()
                .stop_cause,
            Some(RunStopCause::TokenAccountingUnavailable)
        );
        let unbounded_tracker = RunUsageTracker::from_run(store.clone(), &unbounded);
        assert!(unbounded_tracker.record(None).unwrap().is_none());

        for run_id in ["bounded-missing", "unbounded-missing"] {
            let run = store.load_run(run_id).unwrap().unwrap();
            assert!(!run.aggregates.usage_complete);
        }
        assert_eq!(
            store
                .load_run("unbounded-missing")
                .unwrap()
                .unwrap()
                .error_code,
            None
        );
    }

    #[tokio::test]
    async fn bounded_provider_attempts_are_serialized_and_durably_reconciled() {
        let temp = tempfile::tempdir().unwrap();
        let store = OrchStore::open(temp.path().join("orch")).unwrap();
        let run = usage_test_run("bounded-admission", Some(100));
        store.save_run(&run).unwrap();
        let tracker = RunUsageTracker::from_run(store.clone(), &run);

        let first = tracker.begin_attempt().await.unwrap();
        assert_eq!(
            store
                .load_run("bounded-admission")
                .unwrap()
                .unwrap()
                .aggregates
                .usage_pending_requests,
            1
        );
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(25),
            tracker.begin_attempt()
        )
        .await
        .is_err());

        first
            .finish(Some(&CompletionUsage {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
                requests: 1,
            }))
            .unwrap();
        let second = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            tracker.begin_attempt(),
        )
        .await
        .unwrap()
        .unwrap();
        second
            .finish(Some(&CompletionUsage {
                prompt_tokens: 2,
                completion_tokens: 1,
                total_tokens: 3,
                requests: 1,
            }))
            .unwrap();

        let persisted = store.load_run("bounded-admission").unwrap().unwrap();
        assert_eq!(persisted.aggregates.usage_pending_requests, 0);
        assert_eq!(persisted.aggregates.usage.total_tokens, 8);
        assert_eq!(persisted.aggregates.usage.requests, 2);
    }

    fn assert_recovery_event(
        events: &mut EventReceiver,
        session_id: Uuid,
        queued: &PromptQueueEntry,
        steering: &PromptQueueEntry,
    ) {
        let event = events.try_recv().expect("queue recovery event");
        match event {
            SessionUpdate::PromptQueueChanged {
                session_id: event_session_id,
                revision,
                entries,
                action,
                origin,
                changed_entry,
                disposition,
            } => {
                assert_eq!(event_session_id, session_id);
                assert_eq!(revision, 1);
                assert_eq!(action, "recovered");
                assert_eq!(origin, "bridge");
                assert_eq!(changed_entry, None);
                assert_eq!(disposition, Some(SteeringDisposition::Queued));
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].id, steering.id);
                assert_eq!(entries[0].version, steering.version + 1);
                assert_eq!(entries[0].source, "steering_delivery_recovery");
                assert!(entries[0].priority);
                assert_eq!(entries[0].owner, steering.owner);
                assert_eq!(entries[1], *queued);
            }
            other => panic!("unexpected queue recovery event: {other:?}"),
        }
    }

    #[test]
    fn memory_authorization_fails_closed_on_forged_session_agent_binding() {
        let (_home, host, first_lane_id) = test_host();
        let second_lane = host.session_new_kind(SessionKind::Build).unwrap();
        let first_agent = host.ensure_session_agent(first_lane_id).unwrap();
        let second_agent = host.ensure_session_agent(second_lane.id).unwrap();
        assert_ne!(first_agent.agent_id, second_agent.agent_id);

        host.inner
            .lock()
            .sessions
            .get_mut(&first_lane_id)
            .unwrap()
            .agent_id = Some(second_agent.agent_id);

        let error = host
            .memory_list(
                first_lane_id,
                MemoryScope::AgentPrivate {
                    agent_id: first_agent.agent_id,
                },
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("binding mismatch"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn turn_busy_guard_abort_recovery_emits_authoritative_queue_snapshot() {
        let (_home, host, session_id) = test_host();
        let (queued, steering) = {
            let mut g = host.inner.lock();
            g.turn_cancels.insert(session_id, CancellationToken::new());
            let queue = g.prompt_queues.entry(session_id).or_default();
            let queued = queue
                .add("durable follow-up", "composer", false)
                .expect("queue follow-up");
            let steering = queue
                .steer_text_with_owner("recover after abort".into(), true, Some("mcp".into()))
                .expect("queue steering")
                .entry;
            (queued, steering)
        };
        let mut events = host.event_bus().subscribe();

        drop(TurnBusyGuard {
            host: host.clone(),
            session_id,
            armed: true,
        });

        assert!(!host.session_busy(session_id));
        assert_recovery_event(&mut events, session_id, &queued, &steering);
        let entries = host
            .session_queue_list(session_id)
            .expect("recovered queue");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, steering.id);
        assert_eq!(entries[0].version, steering.version + 1);
    }

    /// Make the durable write fail deterministically: `atomic_write_json`
    /// creates `<queue>.json.tmp`, so a directory at that path fails the
    /// create without touching anything else.
    fn block_queue_persistence(session_id: Uuid) {
        let blocked = crate::session_store::session_dir(session_id).join("prompt_queue.json.tmp");
        std::fs::create_dir_all(&blocked).expect("block the queue temp path");
    }

    fn seed_pending_steering(
        host: &AgentHostHandle,
        session_id: Uuid,
    ) -> (PromptQueueEntry, PromptQueueEntry) {
        let mut g = host.inner.lock();
        let queue = g.prompt_queues.entry(session_id).or_default();
        let queued = queue
            .add("durable follow-up", "composer", false)
            .expect("queue follow-up");
        let steering = queue
            .steer_text_with_owner("recover me".into(), true, Some("mcp".into()))
            .expect("queue steering")
            .entry;
        queue.drain_steering();
        (queued, steering)
    }

    /// A durable write failure used to return before the recovery was applied,
    /// so accepted steering stayed in `delivering` where no later boundary
    /// would deliver it and neither the GUI nor `ptah_get_queue` could see it —
    /// and the abort path discarded the error, so nothing said so.
    ///
    /// The interjection must survive in the live queue, and the failure must
    /// be audible.
    #[test]
    fn a_failed_recovery_write_keeps_the_steering_and_reports_the_failure() {
        let (_home, host, session_id) = test_host();
        let (_queued, steering) = seed_pending_steering(&host, session_id);
        block_queue_persistence(session_id);
        let mut events = host.event_bus().subscribe();

        let error = host
            .recover_pending_steering_delivery(session_id)
            .expect_err("a blocked durable write must be reported");
        assert!(
            error.to_string().contains("persist steering recovery"),
            "unexpected error: {error}"
        );

        // Not lost: the interjection is in the live queue where the session
        // can still act on it.
        let entries = host.session_queue_list(session_id).expect("queue readable");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, steering.id);
        assert_eq!(entries[0].source, "steering_delivery_recovery");

        // Not claimed as authoritative: no queue snapshot is published for a
        // mutation that is not durable, so no consumer watermark advances.
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, SessionUpdate::PromptQueueChanged { .. }),
                "an uncommitted recovery must not publish a queue snapshot"
            );
        }
    }

    /// The abort path is the common one and used to swallow the error whole.
    #[test]
    fn the_abort_path_reports_a_failed_recovery_write_too() {
        let (_home, host, session_id) = test_host();
        let (_queued, steering) = seed_pending_steering(&host, session_id);
        block_queue_persistence(session_id);
        let mut events = host.event_bus().subscribe();

        {
            let _guard = TurnBusyGuard {
                host: host.clone(),
                session_id,
                armed: true,
            };
        }

        let reported = std::iter::from_fn(|| events.try_recv().ok()).any(|event| match event {
            SessionUpdate::Error { message, .. } => message.contains("persist steering recovery"),
            SessionUpdate::PromptQueueChanged { .. } => {
                panic!("an uncommitted recovery must not publish a queue snapshot")
            }
            _ => false,
        });
        assert!(
            reported,
            "the abort path must report a failed durable write"
        );

        let entries = host.session_queue_list(session_id).expect("queue readable");
        assert_eq!(entries[0].id, steering.id);
    }

    #[test]
    fn agent_error_recovery_emits_authoritative_queue_snapshot() {
        let (_home, host, session_id) = test_host();
        let (queued, steering) = {
            let mut g = host.inner.lock();
            let queue = g.prompt_queues.entry(session_id).or_default();
            let queued = queue
                .add("durable follow-up", "composer", false)
                .expect("queue follow-up");
            let steering = queue
                .steer_text_with_owner("recover after agent error".into(), true, Some("mcp".into()))
                .expect("queue steering")
                .entry;
            queue.drain_steering();
            (queued, steering)
        };
        let mut events = host.event_bus().subscribe();

        // This is the recovery handler called by the agent-error arm.
        host.recover_pending_steering_delivery(session_id)
            .expect("recover steering after agent error");

        assert_recovery_event(&mut events, session_id, &queued, &steering);
        let entries = host
            .session_queue_list(session_id)
            .expect("recovered queue");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, steering.id);
        assert_eq!(entries[0].version, steering.version + 1);
    }

    #[tokio::test]
    async fn persistent_resume_idempotency_replays_failure_and_rejects_payload_change() {
        let (_home, host, session_id) = test_host();
        let workspace = tempfile::tempdir().unwrap();
        host.set_project_cwd(workspace.path()).unwrap();
        host.session_set_cwd(session_id, workspace.path()).unwrap();
        host.ensure_session_agent(session_id).unwrap();

        let first = host
            .resume_agent_with_request_id(
                session_id,
                "continue from the checkpoint".into(),
                Some(1),
                Some("resume-idempotency-test".into()),
            )
            .await
            .unwrap_err()
            .to_string();
        let replay = host
            .resume_agent_with_request_id(
                session_id,
                "continue from the checkpoint".into(),
                Some(1),
                Some("resume-idempotency-test".into()),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(first.contains("persistent agent has no verified checkpoint"));
        assert!(replay.contains("persistent agent has no verified checkpoint"));

        let changed = host
            .resume_agent_with_request_id(
                session_id,
                "different payload".into(),
                Some(1),
                Some("resume-idempotency-test".into()),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(changed.contains("idempotency") || changed.contains("payload"));
    }

    #[tokio::test]
    async fn persistent_continuation_is_durable_deterministic_and_prompt_bounded() {
        let (_home, host, session_id) = test_host();
        let agent = host.ensure_session_agent(session_id).unwrap();
        let spec = agent.current_spec().unwrap().clone();
        let store = host.ensure_orchestration_store().unwrap();
        let now = Utc::now();
        let run = RunRecord {
            run_id: "continuation-source-run".into(),
            session_id,
            workspace: agent.workspace.clone(),
            request_id: "continuation-source-request".into(),
            client_id: Some("test".into()),
            state: RunState::Completed,
            purpose: RunPurpose::Execution,
            agent_id: Some(agent.agent_id.clone()),
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: Some(spec.revision),
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: spec.default_run_bounds.clone(),
            prompt_preview: "previous finite run".into(),
            start_seq: Some(1),
            end_seq: Some(2),
            created_at: now,
            updated_at: now,
            terminal_result: Some("completed".into()),
            final_response: Some("Continue by implementing deterministic recovery.".into()),
            error_code: None,
            stop_cause: Some(RunStopCause::Completed),
            aggregates: RunAggregates {
                changes: vec![crate::orchestration::ChangeRecord {
                    path: "src/recovery.rs".into(),
                    summary: "Added restart recovery".into(),
                }],
                ..RunAggregates::default()
            },
            progress: Some(crate::orchestration::RunProgress {
                round: 2,
                max_rounds: 4,
                last_tool: Some("apply_patch".into()),
                detail: "Recovery implementation is ready for tests".into(),
                updated_at: now,
            }),
            execution: None,
            approval: None,
        };
        store.save_run(&run).unwrap();
        let mut checkpoint = ContinuationCheckpoint {
            checkpoint_id: "continuation-source-checkpoint".into(),
            agent_id: agent.agent_id.clone(),
            session_id,
            run_id: run.run_id.clone(),
            agent_spec_revision: Some(spec.revision),
            parent_checkpoint_id: None,
            ordinal: 1,
            workspace: agent.workspace.clone(),
            context_summary: "legacy summary is not used for assembly".into(),
            context_hash: String::new(),
            event_seq: 2,
            reason: ContinuationReason::TurnCompleted,
            created_at: now,
        };
        checkpoint.context_hash = checkpoint.context_hash_for();
        store.save_checkpoint(&checkpoint).unwrap();
        store
            .update_agent(&agent.agent_id, |record| {
                record.state = AgentState::Waiting;
                record.current_run_id = None;
                record.last_run_id = Some(run.run_id.clone());
                record.last_lane_id = Some(session_id);
                record.latest_checkpoint_id = Some(checkpoint.checkpoint_id.clone());
                record.continuation_ordinal = checkpoint.ordinal;
                Ok(())
            })
            .unwrap();

        let first = host
            .prepare_agent_continuation(session_id, "continue", Some(3))
            .unwrap();
        let second = host
            .prepare_agent_continuation(session_id, "continue", Some(3))
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.context.prompt_bytes + "continue".len(),
            first.context.rendered_context.len() + "continue".len()
        );
        assert!(
            first.context.prompt_bytes + "continue".len()
                <= first.effective_run_bounds.max_prompt_bytes
        );
        assert_eq!(
            store
                .load_continuation_input(&first.input_snapshot.input_hash)
                .unwrap(),
            Some(first.input_snapshot.clone())
        );
        assert_eq!(
            store
                .load_continuation_context(&first.context.context_id)
                .unwrap(),
            Some(first.context)
        );

        let offline = TestEnvOverride::set("GROKPTAH_AGENT_OFFLINE", "1");
        let response = host
            .resume_agent_with_request_id(
                session_id,
                "continue".into(),
                Some(3),
                Some("deterministic-continuation-replay".into()),
            )
            .await
            .unwrap();
        let run_count = host.list_session_runs(session_id).unwrap().len();
        let replay = host
            .resume_agent_with_request_id(
                session_id,
                "continue".into(),
                Some(3),
                Some("deterministic-continuation-replay".into()),
            )
            .await
            .unwrap();
        drop(offline);
        assert_eq!(replay, response);
        assert_eq!(host.list_session_runs(session_id).unwrap().len(), run_count);
        let receipt = store
            .load_idempotency("deterministic-continuation-replay")
            .unwrap()
            .unwrap();
        let run_id = receipt.run_id.expect("receipt records the finite Run ID");
        let resumed_run = store.load_run(&run_id).unwrap().unwrap();
        assert!(run_id.starts_with("desktop-"));
        assert!(resumed_run.continuation_context_id.is_some());

        let losing_plan = host
            .prepare_agent_continuation(session_id, "must not leak", Some(1))
            .unwrap();
        let current_agent = store
            .load_agent(&losing_plan.agent.agent_id)
            .unwrap()
            .unwrap();
        let now = Utc::now();
        let competing = RunRecord {
            run_id: "competing-admission-run".into(),
            session_id,
            workspace: current_agent.workspace.clone(),
            request_id: "competing-admission-request".into(),
            client_id: Some("test".into()),
            state: RunState::Running,
            purpose: RunPurpose::Execution,
            agent_id: Some(current_agent.agent_id.clone()),
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: Some(current_agent.current_spec().unwrap().revision),
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: current_agent
                .current_spec()
                .unwrap()
                .default_run_bounds
                .clone(),
            prompt_preview: "competing admission".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        store
            .save_run_and_activate_agent(&competing, &current_agent.agent_id)
            .unwrap();
        let transcript_before = host.export_transcript(session_id).unwrap();
        let error = host
            .session_prompt_inner(
                session_id,
                "must not leak".into(),
                Some(1),
                None,
                None,
                Some(losing_plan),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("durable Run"), "unexpected error: {error}");
        assert_eq!(
            host.export_transcript(session_id).unwrap(),
            transcript_before
        );
    }

    #[test]
    fn one_agent_can_own_multiple_lanes_while_archive_preserves_the_binding() {
        let (_home, host, primary_lane_id) = test_host();
        let secondary = host
            .session_new_kind(SessionKind::Build)
            .expect("create second Lane");
        let agent = host
            .ensure_session_agent(primary_lane_id)
            .expect("create durable Agent");

        let attached = host
            .attach_session_to_agent(secondary.id, &agent.agent_id)
            .expect("attach second Lane");
        assert_eq!(attached.known_lane_ids().len(), 2);
        assert!(attached.known_lane_ids().contains(&primary_lane_id));
        assert!(attached.known_lane_ids().contains(&secondary.id));

        let workspace = host.workspace_ui_state();
        assert_eq!(workspace.active_lane_id, Some(secondary.id));
        assert_eq!(workspace.lanes.len(), 2);
        assert!(workspace
            .lanes
            .iter()
            .all(|lane| lane.agent_id.as_deref() == Some(agent.agent_id.as_str())));
        assert_eq!(host.list_lanes(false).len(), 2);

        host.session_archive(secondary.id, true)
            .expect("archive secondary Lane");
        assert_eq!(host.list_lanes(false).len(), 1);
        assert_eq!(host.list_lanes(true).len(), 2);
        let archived_workspace = host.workspace_ui_state();
        let archived_lane = archived_workspace
            .lanes
            .iter()
            .find(|lane| lane.id == secondary.id)
            .expect("archived Lane remains in the archive-aware projection");
        assert!(archived_lane.archived);
        let archived = host
            .list_all_sessions()
            .into_iter()
            .find(|session| session.id == secondary.id)
            .expect("archived Lane remains durable");
        assert!(archived.archived);
        assert_eq!(archived.agent_id.as_deref(), Some(agent.agent_id.as_str()));
    }

    #[test]
    fn local_work_projection_is_lane_scoped_and_redacted() {
        let (_home, host, lane_id) = test_host();
        let other_lane = host
            .session_new_kind(SessionKind::Build)
            .expect("create second Lane");
        let store = host.ensure_orchestration_store().expect("open work store");
        let item = WorkItem::new(
            "implementation",
            "show durable work",
            lane_id,
            "/tmp/project",
            "desktop",
            WorkPolicy::default(),
        )
        .expect("create Work Item");
        store.save_work_item(&item).expect("persist Work Item");

        let visible = host
            .list_work_items_for_session(lane_id)
            .expect("list local Work Items");
        assert_eq!(visible, vec![item.clone()]);
        assert!(host
            .list_work_items_for_session(other_lane.id)
            .expect("list other Lane Work Items")
            .is_empty());

        let snapshot = host
            .get_work_item_snapshot(lane_id, &item.work_id)
            .expect("read Work Item snapshot")
            .expect("Work Item snapshot exists");
        assert_eq!(snapshot.work, item);
        assert!(snapshot.attempts.is_empty());
        assert!(host
            .get_work_item_snapshot(other_lane.id, &snapshot.work.work_id)
            .expect("cross-Lane Work Item read")
            .is_none());
    }

    #[tokio::test]
    async fn archived_lane_inspection_is_read_only_and_new_work_is_rejected() {
        let (_home, host, lane_id) = test_host();
        host.session_queue_add(lane_id, "preserve this queued prompt".into(), false)
            .expect("queue before archive");
        host.session_archive(lane_id, true).expect("archive Lane");

        let before = host.workspace_ui_state();
        let inspected = host
            .session_inspect(lane_id)
            .expect("inspect archived Lane");
        let after = host.workspace_ui_state();

        assert!(inspected.archived);
        assert_eq!(before.active_lane_id, after.active_lane_id);
        assert_eq!(before.active_session, after.active_session);
        assert_eq!(before.open_tab_ids, after.open_tab_ids);
        assert_eq!(before.project_cwd, after.project_cwd);
        assert_eq!(
            host.session_queue_list(lane_id)
                .expect("archived queue remains readable")
                .len(),
            1
        );

        let load_error = host.session_load(lane_id).unwrap_err().to_string();
        assert!(load_error.contains("inspection-only"));
        host.set_open_tabs(vec![lane_id], Some(lane_id));
        let persisted = host.workspace_ui_state();
        assert!(persisted.open_tab_ids.contains(&lane_id));
        assert_ne!(persisted.active_lane_id, Some(lane_id));

        assert!(host.rewind_session(lane_id, 0, "all").is_err());
        assert!(host.compact_session(lane_id).is_err());
        assert!(host.set_plan_mode(lane_id, true).is_err());
        assert!(host
            .reserve_orchestration_turn("archived-run", lane_id)
            .is_err());
        assert!(host
            .reserve_orchestration_queue_slot("archived-pending", lane_id)
            .is_err());
        assert!(host.begin_computer_agent_operation(lane_id).is_err());

        let queue_error = host
            .session_queue_add(lane_id, "must not queue".into(), false)
            .unwrap_err()
            .to_string();
        assert!(queue_error.contains("inspection-only"));
        let drain_error = host
            .session_queue_take_next(lane_id)
            .unwrap_err()
            .to_string();
        assert!(drain_error.contains("inspection-only"));
        let cwd_error = host
            .session_set_cwd(lane_id, std::env::temp_dir())
            .unwrap_err()
            .to_string();
        assert!(cwd_error.contains("inspection-only"));
        let mode_error = host
            .session_set_execution_mode(lane_id, RunExecutionMode::IsolatedWorktree)
            .unwrap_err()
            .to_string();
        assert!(mode_error.contains("inspection-only"));
        let prompt_error = host
            .session_prompt(lane_id, "must not run".into())
            .await
            .unwrap_err()
            .to_string();
        assert!(prompt_error.contains("inspection-only"));
    }

    #[test]
    fn failed_workspace_promotion_preserves_the_previous_lane_scope() {
        let (_home, host, first_lane_id) = test_host();
        let first_workspace = tempfile::tempdir().unwrap();
        host.session_set_cwd(first_lane_id, first_workspace.path())
            .expect("bind first Lane workspace");
        let second = host
            .session_new_kind(SessionKind::Build)
            .expect("create second Lane");
        let missing = first_workspace.path().join("workspace-that-does-not-exist");
        {
            let mut inner = host.inner.lock();
            inner.sessions.get_mut(&second.id).unwrap().cwd = missing;
        }
        host.session_load(first_lane_id)
            .expect("promote first Lane workspace");

        let before = host.workspace_ui_state();
        let error = host.session_load(second.id).unwrap_err().to_string();
        let after = host.workspace_ui_state();

        assert!(error.contains("workspace is missing"));
        assert_eq!(after.active_lane_id, before.active_lane_id);
        assert_eq!(after.project_cwd, before.project_cwd);
    }

    #[test]
    fn persistent_agent_authority_can_narrow_but_ambient_settings_cannot_widen_it() {
        let (_home, host, lane_id) = test_host();
        host.set_permission_mode("default".into());
        host.set_sandbox("read-only".into());
        let agent = host.ensure_session_agent(lane_id).unwrap();
        let captured_model = agent.model.clone();
        let captured = &agent.current_spec().unwrap().authority;
        assert!(!captured.bypass_permissions);
        assert!(sandbox_is_readonly(&captured.sandbox_profile));

        host.set_permission_mode("bypassPermissions".into());
        host.set_sandbox("full".into());
        assert_eq!(
            host.ambient_tool_gate("run_terminal_cmd"),
            ToolGate::AutoAllow
        );
        assert_eq!(
            host.tool_gate(lane_id, "run_terminal_cmd"),
            ToolGate::Prompt
        );
        assert_eq!(
            host.tool_gate(lane_id, "future_unreviewed_mutator"),
            ToolGate::AutoDeny
        );
        assert!(host.session_sandbox_is_readonly(lane_id));
        assert!(host
            .computer_agent_eligibility(lane_id)
            .unwrap_err()
            .to_string()
            .contains("not allowed by this Agent specification"));

        host.set_model("grok-a-different-focused-model".into());
        let reloaded = host.ensure_session_agent(lane_id).unwrap();
        assert_eq!(reloaded.model, captured_model);
        assert_eq!(
            reloaded.current_spec().unwrap().model.selection_key,
            captured_model
        );

        host.set_allow_deny_rules(Vec::new(), vec!["run_terminal_cmd".into()]);
        assert_eq!(
            host.tool_gate(lane_id, "run_terminal_cmd"),
            ToolGate::AutoDeny
        );
    }

    #[tokio::test]
    async fn active_run_keeps_its_frozen_spec_authority_after_revision() {
        let (_home, host, lane_id) = test_host();
        host.set_permission_mode("default".into());
        let agent = host.ensure_session_agent(lane_id).unwrap();
        let revision = agent.current_spec().unwrap().revision;
        let store = host.ensure_orchestration_store().unwrap();
        let now = Utc::now();
        let run = RunRecord {
            run_id: "frozen-spec-run".into(),
            session_id: lane_id,
            workspace: agent.workspace.clone(),
            request_id: "frozen-spec-request".into(),
            client_id: Some("test".into()),
            state: RunState::Running,
            purpose: RunPurpose::Execution,
            agent_id: Some(agent.agent_id.clone()),
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: Some(revision),
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: agent.current_spec().unwrap().default_run_bounds.clone(),
            prompt_preview: "freeze policy".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        store
            .save_run_and_activate_agent(&run, &agent.agent_id)
            .unwrap();
        host.reserve_orchestration_turn(&run.run_id, lane_id)
            .unwrap();
        store
            .revise_agent_spec(&agent.agent_id, "test:deny-mid-run", |spec| {
                spec.authority.deny_rules.push("run_terminal_cmd".into());
                Ok(())
            })
            .unwrap();

        assert_eq!(
            host.tool_gate(lane_id, "run_terminal_cmd"),
            ToolGate::Prompt
        );
        let frozen = host.session_agent_spec(lane_id).unwrap().unwrap();
        assert_eq!(frozen.revision, revision);
        let error = host
            .session_prompt_reserved_with_max_rounds_for_run(
                lane_id,
                "must not start under a newer specification".into(),
                Some(1),
                &run.run_id,
                &run.run_id,
                RunExecutionMode::Shared,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("specification changed before external turn start"),
            "unexpected error: {error}"
        );
        host.release_orchestration_turn(&run.run_id);
    }

    #[test]
    fn manager_decision_run_is_host_enforced_proposal_only() {
        let (_home, host, lane_id) = test_host();
        host.set_permission_mode("bypassPermissions".into());
        let agent = host.ensure_session_agent(lane_id).unwrap();
        let revision = agent.current_spec().unwrap().revision;
        let store = host.ensure_orchestration_store().unwrap();
        let now = Utc::now();
        let mut work = WorkItem::new(
            "manager-decision",
            "return a typed proposal",
            lane_id,
            agent.workspace.clone(),
            "manager-supervisor",
            WorkPolicy::default(),
        )
        .unwrap();
        work.assigned_agent_id = Some(agent.agent_id.clone());
        work.assignment_status = crate::orchestration::AssignmentStatus::Accepted;
        work.parent_work_id = Some("manager-root".into());
        work.source_manager_plan_id = Some("manager-plan".into());
        work.source_manager_step_id = Some("__manager_decision__".into());
        work.validate().unwrap();
        store.save_work_item(&work).unwrap();
        let run = RunRecord {
            run_id: "manager-proposal-run".into(),
            session_id: lane_id,
            workspace: agent.workspace.clone(),
            request_id: "manager-proposal-intent".into(),
            client_id: Some("native-executor".into()),
            state: RunState::Running,
            purpose: RunPurpose::ManagerProposal,
            agent_id: Some(agent.agent_id.clone()),
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: Some(revision),
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: agent.current_spec().unwrap().default_run_bounds.clone(),
            prompt_preview: "typed proposal".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        store
            .save_run_and_activate_agent(&run, &agent.agent_id)
            .unwrap();
        host.reserve_orchestration_turn(&run.run_id, lane_id)
            .unwrap();
        host.run_usage_trackers
            .lock()
            .insert(lane_id, RunUsageTracker::from_run(store.clone(), &run));

        assert_eq!(
            host.tool_gate(lane_id, "run_terminal_cmd"),
            ToolGate::AutoDeny
        );
        assert_eq!(host.tool_gate(lane_id, "mcp"), ToolGate::AutoDeny);
    }

    #[test]
    fn checkpoint_failure_still_deactivates_the_terminal_agent() {
        let (_home, host, lane_id) = test_host();
        let mut agent = host.ensure_session_agent(lane_id).unwrap();
        let store = host.ensure_orchestration_store().unwrap();
        let missing_lane = Uuid::new_v4();
        agent.agent_id = "checkpoint-failure-agent".into();
        agent.session_id = missing_lane;
        agent.lane_ids = vec![missing_lane];
        agent.lane_associations = vec![AgentLaneAssociation {
            lane_id: missing_lane,
            source_workspace: agent.workspace.clone(),
            attached_at: Utc::now(),
            attached_by: "test".into(),
            detached_at: None,
            detached_by: None,
        }];
        agent.current_run_id = None;
        agent.state = AgentState::Waiting;
        store.save_agent(&agent).unwrap();

        let mut run = usage_test_run("checkpoint-failure-run", Some(100));
        run.session_id = missing_lane;
        run.workspace = agent.workspace.clone();
        run.agent_id = Some(agent.agent_id.clone());
        run.agent_spec_revision = Some(agent.current_spec().unwrap().revision);
        store
            .save_run_and_activate_agent(&run, &agent.agent_id)
            .unwrap();
        let error = host
            .persist_agent_checkpoint(&run, "failed", 1, &host.event_bus(), &store)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("unknown session"),
            "unexpected error: {error}"
        );
        let deactivated = store.load_agent(&agent.agent_id).unwrap().unwrap();
        assert_eq!(deactivated.current_run_id, None);
        assert_eq!(
            deactivated.last_run_id.as_deref(),
            Some(run.run_id.as_str())
        );
        assert_eq!(deactivated.state, AgentState::Failed);
    }

    #[test]
    fn agent_memory_access_obeys_current_spec_scope_membership() {
        let (_home, host, lane_id) = test_host();
        let agent = host.ensure_session_agent(lane_id).unwrap();
        let store = host.ensure_orchestration_store().unwrap();
        store
            .revise_agent_spec(&agent.agent_id, "test:memory-policy", |spec| {
                spec.memory.project_scope = false;
                spec.memory.agent_private_scope = false;
                spec.memory.team_ids = vec!["design-team".into()];
                Ok(())
            })
            .unwrap();

        assert!(host.memory_list(lane_id, MemoryScope::Project).is_err());
        assert!(host
            .memory_list(
                lane_id,
                MemoryScope::AgentPrivate {
                    agent_id: agent.agent_id,
                },
            )
            .is_err());
        assert!(host
            .memory_list(
                lane_id,
                MemoryScope::Team {
                    team_id: "design-team".into(),
                },
            )
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn persistent_agent_prompt_and_duration_bounds_are_enforced_as_recorded() {
        let (_home, host, lane_id) = test_host();
        let agent = host.ensure_session_agent(lane_id).unwrap();
        let store = host.ensure_orchestration_store().unwrap();
        store
            .revise_agent_spec(&agent.agent_id, "test:prompt-bound", |spec| {
                spec.default_run_bounds.max_prompt_bytes = 4;
                Ok(())
            })
            .unwrap();
        let error = host
            .session_prompt(lane_id, "too long".into())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("max_prompt_bytes"));
        assert!(host.list_session_runs(lane_id).unwrap().is_empty());

        store
            .revise_agent_spec(&agent.agent_id, "test:duration-bound", |spec| {
                spec.default_run_bounds.max_prompt_bytes = 10_000;
                spec.default_run_bounds.max_duration_ms = 50;
                Ok(())
            })
            .unwrap();
        let offline = TestEnvOverride::set("GROKPTAH_AGENT_OFFLINE", "1");
        let outcome = host.session_prompt(lane_id, "run sleep 5".into()).await;
        drop(offline);
        let error = outcome.unwrap_err().to_string();
        assert!(error.contains("duration limit"));
        let runs = host.list_session_runs(lane_id).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, RunState::LimitReached);
        assert_eq!(runs[0].stop_cause, Some(RunStopCause::DurationLimit));
        assert_eq!(runs[0].bounds.max_duration_ms, 50);
    }
}
