//! The headless host: startup, admission, stepping, control, and shutdown.
//!
//! The host is the authority for exactly one session, one workspace, and one
//! home that it owns exclusively. It is deliberately not a general service: it
//! originates no work of its own, exposes no listening socket, and holds no
//! credential. What it adds over the desktop authority is that its runs survive
//! the operator's terminal closing, and that every decision it cannot make on
//! its own stops the run and asks.

use std::collections::BTreeMap;
use std::sync::Arc;

use grokptah_agent_sdk::RunScope;
use grokptah_agent_sdk::run::{
    Bounds, ExecutionMode, MAX_PROMPT_PREVIEW_BYTES, MAX_REVIEW_DIFF_BYTES, SubmitTaskRequest,
};
use serde_json::{Value, json};

use crate::attention::{AttentionKind, AttentionRecord, AttentionResolution};
use crate::authority::{
    Authority, CAP_EXECUTE, CAP_OBSERVE, CAP_PROMOTE, CAP_QUEUE, CAP_RESUME, CAP_REVIEW,
};
use crate::clock::Clock;
use crate::config::{EngineSelection, HostConfig};
use crate::control::{ControlCommand, ControlReply, ControlRequest, parse_request};
use crate::engine::{DispatchDisposition, DispatchReport, EngineOutcome, EngineStep, RunEngine};
use crate::error::{HostError, HostResult};
use crate::identity::{fingerprint, opaque_id};
use crate::journal::CursorStatus;
use crate::lease::{ControlClass, LeaseBook};
use crate::lifecycle::{CancelSignal, HostState, ShutdownKind, ShutdownSignal};
use crate::lock::HomeLock;
use crate::projection::{self, HealthReport, HostRunStatus};
use crate::redaction::{RedactionPolicy, relative_path};
use crate::store::{
    ChangedFileRecord, CompletionRecord, DispatchRecord, RecoveryReport, RunPhase, RunRecord, Store,
};

/// Maximum steering directives held for one run.
pub const MAX_PENDING_STEERING: usize = 16;
/// Maximum bytes accepted in one steering directive.
pub const MAX_STEERING_BYTES: usize = 2 * 1024;
/// Maximum repository-relative path bytes accepted from an engine.
pub const MAX_CHANGED_PATH_BYTES: usize = 512;

/// What starting the host had to repair.
#[derive(Debug, Clone)]
pub struct StartupReport {
    /// Restart recovery outcome.
    pub recovery: RecoveryReport,
    /// Health immediately after start.
    pub health: HealthReport,
}

/// What stopping the host did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    /// How the host was asked to stop.
    pub kind: ShutdownKind,
    /// Runs checkpointed to `paused` by a graceful stop.
    pub paused: Vec<String>,
    /// Runs left live, to be recovered on the next start.
    pub left_live: Vec<String>,
}

/// A durable, observable, steerable headless host.
pub struct HeadlessHost {
    config: HostConfig,
    authority: Authority,
    redaction: RedactionPolicy,
    store: Store,
    leases: LeaseBook,
    engine: Option<Box<dyn RunEngine>>,
    clock: Arc<dyn Clock>,
    shutdown: ShutdownSignal,
    cancel: CancelSignal,
    prompts: BTreeMap<String, String>,
    state: HostState,
    started_at: String,
    started_at_ms: u64,
    recovery: RecoveryReport,
    _lock: HomeLock,
}

impl std::fmt::Debug for HeadlessHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeadlessHost")
            .field("state", &self.state)
            .field("session_id", &self.config.session_id)
            .field("engine", &self.engine_label())
            .finish_non_exhaustive()
    }
}

impl HeadlessHost {
    /// Validate configuration, take the home lock, recover, and become ready.
    ///
    /// The lock is taken before any recovery write, so two hosts can never race
    /// to repair the same home.
    pub fn open(
        config: HostConfig,
        engine: Option<Box<dyn RunEngine>>,
        clock: Arc<dyn Clock>,
        shutdown: ShutdownSignal,
    ) -> HostResult<Self> {
        config.validate()?;
        let started_at_ms = clock.now_ms();
        let started_at = clock.now_rfc3339();

        let lock = HomeLock::acquire(
            &config.home,
            &format!("pid={} started={started_at}", std::process::id()),
        )?;
        let (store, recovery) = Store::open(
            &config.home,
            config.limits.event_retention as usize,
            &started_at,
        )?;

        let authority = Authority::new(&config);
        let redaction = RedactionPolicy::new(config.home_str(), config.workspace_str());

        let mut host = Self {
            config,
            authority,
            redaction,
            store,
            leases: LeaseBook::new(),
            engine,
            clock,
            shutdown,
            cancel: CancelSignal::new(),
            prompts: BTreeMap::new(),
            state: HostState::Ready,
            started_at,
            started_at_ms,
            recovery,
            _lock: lock,
        };

        // A dispatch that never came back is escalated at start rather than
        // left as a quiet phase change: it is the one condition an operator
        // must reconcile against the orchestrator before any work continues.
        for run_id in host.recovery.indeterminate_dispatch.clone() {
            if host.store.get(&run_id)?.phase.is_terminal() {
                continue;
            }
            host.attach_attention(
                &run_id,
                AttentionKind::DispatchUncertain,
                "dispatch_indeterminate",
                "a dispatch was in flight when the host stopped; reconcile it before continuing",
            )?;
        }
        Ok(host)
    }

    /// Cancellation channel handed to every engine step.
    ///
    /// Exposed so an OS signal watcher can ask an in-flight step to stop while
    /// the control loop is blocked inside it.
    pub fn cancel_signal(&self) -> CancelSignal {
        self.cancel.clone()
    }

    /// Report what start had to repair, alongside current health.
    pub fn startup_report(&self) -> StartupReport {
        StartupReport {
            recovery: self.recovery.clone(),
            health: self.health(),
        }
    }

    /// Current lifecycle state.
    pub fn state(&self) -> HostState {
        self.state
    }

    /// The shared shutdown signal, for an OS signal watcher.
    pub fn shutdown_signal(&self) -> ShutdownSignal {
        self.shutdown.clone()
    }

    /// Health and readiness.
    pub fn health(&self) -> HealthReport {
        projection::health(
            &self.config,
            self.state,
            &self.started_at,
            self.clock.now_ms().saturating_sub(self.started_at_ms),
            self.engine_label(),
            true,
            self.leases.len(),
            &self.store,
            self.authority.permitted_ids(),
        )
    }

    fn engine_label(&self) -> &'static str {
        self.engine.as_ref().map_or("none", |engine| engine.label())
    }

    /// Handle one parsed operator request. Never panics; a refusal is a reply.
    pub fn handle(&mut self, request: ControlRequest) -> ControlReply {
        let id = request.id.clone();
        match self.dispatch(request.command) {
            Ok(payload) => ControlReply::ok(id, payload),
            Err(error) => ControlReply::error(id, &error),
        }
    }

    /// Handle one NDJSON request line.
    pub fn handle_line(&mut self, line: &str) -> ControlReply {
        match parse_request(line) {
            Ok(request) => self.handle(request),
            Err(error) => ControlReply::error(None, &error),
        }
    }

    fn dispatch(&mut self, command: ControlCommand) -> HostResult<Value> {
        match command {
            ControlCommand::Health => {
                self.authority.require(CAP_OBSERVE)?;
                to_value(&self.health())
            }
            ControlCommand::Capabilities => {
                self.authority.require(CAP_OBSERVE)?;
                to_value(&json!({
                    "advertised": self.authority.capabilities(),
                    "permitted": self.authority.permitted_ids(),
                }))
            }
            ControlCommand::Submit {
                request_id,
                prompt,
                bounds,
                execution_mode,
                allow_queue,
            } => self.submit(
                &request_id,
                &prompt,
                bounds.as_ref(),
                execution_mode,
                allow_queue.unwrap_or(false),
            ),
            ControlCommand::Status { run_id } => {
                self.authority.require(CAP_OBSERVE)?;
                to_value(&self.status(&run_id)?)
            }
            ControlCommand::Events {
                run_id,
                after_seq,
                limit,
            } => self.events(&run_id, after_seq, limit),
            ControlCommand::Lease {
                run_id,
                classes,
                expected_revision,
                ttl_ms,
            } => self.lease(&run_id, classes, expected_revision, ttl_ms),
            ControlCommand::Steer {
                run_id,
                lease_id,
                expected_revision,
                directive,
            } => self.steer(&run_id, &lease_id, expected_revision, &directive),
            ControlCommand::Pause {
                run_id,
                lease_id,
                expected_revision,
            } => self.pause(&run_id, &lease_id, expected_revision),
            ControlCommand::Resume {
                run_id,
                lease_id,
                expected_revision,
                prompt,
            } => self.resume(&run_id, &lease_id, expected_revision, prompt),
            ControlCommand::Cancel {
                run_id,
                lease_id,
                expected_revision,
            } => self.cancel(&run_id, &lease_id, expected_revision),
            ControlCommand::Attention { run_id } => {
                self.authority.require(CAP_OBSERVE)?;
                to_value(&json!({
                    "runId": run_id,
                    "attention": self.store.get(&run_id)?.attention.clone(),
                }))
            }
            ControlCommand::ResolveAttention {
                run_id,
                attention_id,
                resolution,
            } => self.resolve_attention(&run_id, &attention_id, resolution),
            ControlCommand::Receipt { run_id } => {
                self.authority.require(CAP_REVIEW)?;
                to_value(&projection::review_receipt(self.store.get(&run_id)?)?)
            }
            ControlCommand::Tick { steps } => {
                self.authority.require(CAP_EXECUTE)?;
                let advanced = self.tick(steps.unwrap_or(1))?;
                to_value(&json!({ "advanced": advanced }))
            }
            ControlCommand::Shutdown { immediate } => {
                self.authority.require(CAP_EXECUTE)?;
                let kind = if immediate.unwrap_or(false) {
                    ShutdownKind::Immediate
                } else {
                    ShutdownKind::Graceful
                };
                let state = self.shutdown.request(kind);
                self.state = HostState::Draining;
                to_value(&json!({ "shutdown": state.label() }))
            }
        }
    }

    // ---------------------------------------------------------------- submit

    fn submit(
        &mut self,
        request_id: &str,
        prompt: &str,
        bounds: Option<&Bounds>,
        execution_mode: Option<ExecutionMode>,
        allow_queue: bool,
    ) -> HostResult<Value> {
        self.authority.require(CAP_EXECUTE)?;

        let workspace = self.config.workspace_alias();
        let request = SubmitTaskRequest {
            request_id: request_id.to_owned(),
            session_id: self.config.session_id.clone(),
            workspace: workspace.clone(),
            prompt: prompt.to_owned(),
            bounds: bounds.cloned(),
            execution_mode,
            allow_queue: Some(allow_queue),
        };
        request.validate().map_err(|reason| {
            HostError::invalid("submit_invalid", format!("submit is not valid: {reason}"))
                .with_request_id(request_id)
        })?;

        let resolved = self
            .authority
            .admit_bounds(bounds)
            .map_err(|error| error.with_request_id(request_id))?;
        if prompt.len() > resolved.max_prompt_bytes as usize {
            return Err(HostError::invalid(
                "prompt_too_large",
                "prompt exceeds the admitted bound",
            )
            .with_request_id(request_id));
        }

        let request_fingerprint = fingerprint(&[
            request_id,
            &self.config.session_id,
            &workspace,
            prompt,
            &resolved.max_rounds.to_string(),
            &resolved.max_prompt_bytes.to_string(),
            &resolved.max_duration_ms.to_string(),
        ]);
        if let Some(entry) = self.store.ledger_lookup(request_id) {
            if entry.fingerprint != request_fingerprint {
                return Err(HostError::invalid(
                    "idempotency_conflict",
                    "this request id was already used with different content",
                )
                .with_request_id(request_id));
            }
            let run_id = entry.run_id.clone();
            let status = self.status(&run_id)?;
            return to_value(&json!({ "run": status, "replayed": true }));
        }

        if self.engine.is_none() {
            return Err(HostError::unavailable(
                "engine_disabled",
                "this host has no run engine configured",
            )
            .with_request_id(request_id));
        }

        // Every admitted run enters the queue and is promoted by a later tick,
        // so the queue bound applies whether or not a slot is busy right now.
        let running = self.store.count_phase(RunPhase::Running);
        let queued = self.store.count_phase(RunPhase::Queued);
        if running >= self.config.limits.max_active_runs as usize && !allow_queue {
            return Err(
                HostError::capacity("admission_full", "no execution slot is free")
                    .with_request_id(request_id),
            );
        }
        if queued >= self.config.limits.max_queued_runs as usize {
            return Err(
                HostError::capacity("queue_full", "the admission queue is full")
                    .with_request_id(request_id),
            );
        }

        let now = self.clock.now_rfc3339();
        let run_id = opaque_id("run", &[&self.config.session_id, &workspace, request_id]);
        let (prompt_preview, _) = self
            .redaction
            .scrub_bounded(prompt, MAX_PROMPT_PREVIEW_BYTES);

        let record = RunRecord {
            run_id: run_id.clone(),
            session_id: self.config.session_id.clone(),
            workspace,
            request_id: request_id.to_owned(),
            phase: RunPhase::Queued,
            prompt_preview,
            request_fingerprint: request_fingerprint.clone(),
            created_at: now.clone(),
            updated_at: now,
            revision: 1,
            rounds_used: 0,
            bounds: resolved,
            execution_mode: execution_mode.unwrap_or(ExecutionMode::IsolatedWorktree),
            started_at_ms: None,
            pending_steering: Vec::new(),
            attention: None,
            stop_reason: None,
            completion: None,
            dispatch: None,
        };
        self.store.insert(record)?;
        self.store
            .ledger_record(request_id, &run_id, &request_fingerprint)?;
        self.prompts.insert(run_id.clone(), prompt.to_owned());
        self.append_event(&run_id, "run.admitted", json!({}))?;

        to_value(&json!({ "run": self.status(&run_id)?, "replayed": false }))
    }

    // ----------------------------------------------------------- observation

    fn status(&self, run_id: &str) -> HostResult<HostRunStatus> {
        projection::run_status(self.store.get(run_id)?, self.store.journal(run_id)?)
    }

    fn events(
        &mut self,
        run_id: &str,
        after_seq: Option<u64>,
        limit: Option<u32>,
    ) -> HostResult<Value> {
        self.authority.require(CAP_OBSERVE)?;
        let scope = self.store.get(run_id)?.scope();
        let journal = self.store.journal(run_id)?;
        let status = journal.cursor_status(after_seq);
        let page = journal.page(after_seq, limit.unwrap_or(64) as usize);
        let range = journal.retained_range();

        let recovery = match status {
            CursorStatus::Exact => None,
            CursorStatus::Expired => Some(projection::recovery_notification(
                scope.clone(),
                after_seq.unwrap_or(0),
                "cursor_expired",
            )),
            CursorStatus::Ahead => Some(projection::recovery_notification(
                scope.clone(),
                after_seq.unwrap_or(0),
                "cursor_ahead",
            )),
        };

        to_value(&json!({
            "scope": scope,
            "page": page,
            "eventRange": range,
            "recovery": recovery,
        }))
    }

    // --------------------------------------------------------------- control

    fn lease(
        &mut self,
        run_id: &str,
        classes: Vec<ControlClass>,
        expected_revision: u64,
        ttl_ms: Option<u64>,
    ) -> HostResult<Value> {
        for class in &classes {
            self.authority.require(capability_for(*class))?;
        }
        let ceiling = self.config.limits.lease_ttl_ms;
        let ttl = match ttl_ms {
            Some(value) if value > ceiling => {
                return Err(HostError::invalid(
                    "bounds_exceed_ceiling",
                    "ttlMs exceeds the host lease ceiling",
                ));
            }
            Some(value) => value,
            None => ceiling,
        };

        let record = self.store.get(run_id)?;
        if record.revision != expected_revision {
            return Err(HostError::stale(
                "revision_stale",
                "the run advanced past the observed revision",
            ));
        }
        if record.phase.is_terminal() {
            return Err(HostError::invalid(
                "run_terminal",
                "a terminal run cannot be controlled",
            ));
        }
        let scope = record.scope();
        let revision = record.revision;
        let lease = self
            .leases
            .grant(scope, classes, revision, ttl, self.clock.now_ms())?;
        to_value(&lease)
    }

    fn authorize(
        &mut self,
        run_id: &str,
        lease_id: &str,
        class: ControlClass,
        expected_revision: u64,
    ) -> HostResult<()> {
        let record = self.store.get(run_id)?;
        let scope = record.scope();
        let revision = record.revision;
        self.leases.authorize(
            lease_id,
            &scope,
            class,
            expected_revision,
            revision,
            self.clock.now_ms(),
        )
    }

    fn steer(
        &mut self,
        run_id: &str,
        lease_id: &str,
        expected_revision: u64,
        directive: &str,
    ) -> HostResult<Value> {
        self.authority.require(CAP_QUEUE)?;
        self.authorize(run_id, lease_id, ControlClass::Steer, expected_revision)?;

        let trimmed = directive.trim();
        if trimmed.is_empty() || directive.len() > MAX_STEERING_BYTES {
            return Err(HostError::invalid(
                "directive_invalid",
                "a steering directive must be non-empty and bounded",
            ));
        }
        let (scrubbed, _) = self.redaction.scrub_bounded(trimmed, MAX_STEERING_BYTES);

        let now = self.clock.now_rfc3339();
        let record = self.store.get_mut(run_id)?;
        if !matches!(
            record.phase,
            RunPhase::Queued | RunPhase::Running | RunPhase::Paused
        ) {
            return Err(HostError::invalid(
                "run_not_steerable",
                "only an admitted, non-terminal run can be steered",
            ));
        }
        if record.pending_steering.len() >= MAX_PENDING_STEERING {
            return Err(HostError::capacity(
                "steering_full",
                "too many steering directives are already pending",
            ));
        }
        record.pending_steering.push(scrubbed);
        let phase = record.phase;
        record.transition(phase, now);
        let pending = record.pending_steering.len();
        self.store.persist_record(run_id)?;
        self.append_event(run_id, "run.steered", json!({ "pending": pending }))?;
        to_value(&self.status(run_id)?)
    }

    fn pause(&mut self, run_id: &str, lease_id: &str, expected_revision: u64) -> HostResult<Value> {
        self.authority.require(CAP_EXECUTE)?;
        self.authorize(run_id, lease_id, ControlClass::Pause, expected_revision)?;

        let now = self.clock.now_rfc3339();
        let record = self.store.get_mut(run_id)?;
        if !matches!(record.phase, RunPhase::Queued | RunPhase::Running) {
            return Err(HostError::invalid(
                "run_not_pausable",
                "only a queued or running run can be paused",
            ));
        }
        record.transition(RunPhase::Paused, now);
        record.stop_reason = Some("operator_pause".to_owned());
        self.store.persist_record(run_id)?;
        self.leases.revoke_run(run_id);
        self.append_event(run_id, "run.paused", json!({}))?;
        to_value(&self.status(run_id)?)
    }

    fn resume(
        &mut self,
        run_id: &str,
        lease_id: &str,
        expected_revision: u64,
        prompt: Option<String>,
    ) -> HostResult<Value> {
        self.authority.require(CAP_RESUME)?;
        self.authorize(run_id, lease_id, ControlClass::Resume, expected_revision)?;

        let record = self.store.get(run_id)?;
        // An unsettled dispatch outranks every other blocker: resuming would
        // re-run a round that may already have taken effect elsewhere, and no
        // operator action short of reconciling can make that safe.
        if record.dispatch_blocks_progress() {
            return Err(indeterminate_dispatch());
        }
        // An open escalation is checked next: it is the blocker the operator
        // has to clear, and reporting the phase instead would send them looking
        // at the wrong thing.
        if record.attention.is_some() {
            return Err(HostError::forbidden(
                "attention_open",
                "resolve the run's escalation before resuming it",
            ));
        }
        if !matches!(record.phase, RunPhase::Paused | RunPhase::Interrupted) {
            return Err(HostError::invalid(
                "run_not_resumable",
                "only a paused or interrupted run can be resumed",
            ));
        }
        let bound = record.bounds.max_prompt_bytes as usize;

        // The full prompt is never durable, so a run recovered from a restart
        // needs the operator to restate it. This is the same manual-resume rule
        // the desktop authority already applies.
        let resolved_prompt = match prompt {
            Some(prompt) => {
                if prompt.trim().is_empty() || prompt.len() > bound {
                    return Err(HostError::invalid(
                        "prompt_invalid",
                        "the resume prompt must be non-empty and within the admitted bound",
                    ));
                }
                prompt
            }
            None => self.prompts.get(run_id).cloned().ok_or_else(|| {
                HostError::invalid(
                    "prompt_required",
                    "this run was recovered from a restart and needs an explicit prompt",
                )
            })?,
        };

        let now = self.clock.now_rfc3339();
        let record = self.store.get_mut(run_id)?;
        record.transition(RunPhase::Queued, now);
        record.stop_reason = None;
        self.store.persist_record(run_id)?;
        self.prompts.insert(run_id.to_owned(), resolved_prompt);
        self.leases.revoke_run(run_id);
        self.append_event(run_id, "run.resumed", json!({}))?;
        to_value(&self.status(run_id)?)
    }

    fn cancel(
        &mut self,
        run_id: &str,
        lease_id: &str,
        expected_revision: u64,
    ) -> HostResult<Value> {
        self.authority.require(CAP_EXECUTE)?;
        self.authorize(run_id, lease_id, ControlClass::Cancel, expected_revision)?;

        let now = self.clock.now_rfc3339();
        let record = self.store.get_mut(run_id)?;
        if record.phase.is_terminal() {
            return Err(HostError::invalid(
                "run_terminal",
                "a terminal run cannot be cancelled",
            ));
        }
        record.transition(RunPhase::Cancelled, now);
        record.stop_reason = Some("operator_cancel".to_owned());
        record.pending_steering.clear();
        record.attention = None;
        self.store.persist_record(run_id)?;
        self.leases.revoke_run(run_id);
        self.prompts.remove(run_id);
        self.append_event(run_id, "run.cancelled", json!({}))?;
        to_value(&self.status(run_id)?)
    }

    fn resolve_attention(
        &mut self,
        run_id: &str,
        attention_id: &str,
        resolution: AttentionResolution,
    ) -> HostResult<Value> {
        // Allowing a halted run to proceed is a human gate, so it is held to
        // the gated capability rather than the ordinary execute capability.
        // Denying only stops work, so it needs no gate beyond execute.
        match resolution {
            AttentionResolution::Allow => self.authority.require(CAP_PROMOTE)?,
            AttentionResolution::Deny => self.authority.require(CAP_EXECUTE)?,
        }

        let now_ms = self.clock.now_ms();
        let now = self.clock.now_rfc3339();
        let record = self.store.get(run_id)?;
        let attention = record.attention.clone().ok_or_else(|| {
            HostError::not_found("attention_absent", "this run has no open escalation")
        })?;
        attention.ensure_matches(attention_id)?;
        if attention.is_expired(now_ms) {
            return Err(HostError::stale(
                "attention_expired",
                "the escalation passed its deadline and is denied",
            ));
        }

        if resolution == AttentionResolution::Allow && record.dispatch_blocks_progress() {
            return Err(indeterminate_dispatch());
        }

        let record = self.store.get_mut(run_id)?;
        record.attention = None;
        match resolution {
            AttentionResolution::Allow => {
                record.transition(RunPhase::Queued, now);
                record.stop_reason = None;
            }
            AttentionResolution::Deny => {
                record.transition(RunPhase::Failed, now);
                record.stop_reason = Some("attention_denied".to_owned());
            }
        }
        self.store.persist_record(run_id)?;
        self.append_event(
            run_id,
            "run.attention_resolved",
            json!({
                "attentionId": attention.attention_id,
                "kind": attention.kind.label(),
                "resolution": resolution,
            }),
        )?;
        to_value(&self.status(run_id)?)
    }

    // ------------------------------------------------------------- execution

    /// Advance the host by at most `steps` engine steps.
    pub fn tick(&mut self, steps: u32) -> HostResult<usize> {
        let mut advanced = 0usize;
        for _ in 0..steps {
            self.leases.expire(self.clock.now_ms());
            self.expire_attention()?;
            self.promote_queued()?;
            if !self.advance_one()? {
                break;
            }
            advanced += 1;
        }
        Ok(advanced)
    }

    fn expire_attention(&mut self) -> HostResult<()> {
        let now_ms = self.clock.now_ms();
        let now = self.clock.now_rfc3339();
        let expired: Vec<(String, String)> = self
            .store
            .records()
            // A terminal run's outcome is final. An escalation left on one is
            // still worth clearing, but it must never turn a completed run into
            // a failed one.
            .filter(|record| !record.phase.is_terminal())
            .filter_map(|record| {
                record
                    .attention
                    .as_ref()
                    .filter(|attention| attention.is_expired(now_ms))
                    .map(|attention| (record.run_id.clone(), attention.attention_id.clone()))
            })
            .collect();

        for (run_id, attention_id) in expired {
            let record = self.store.get_mut(&run_id)?;
            record.attention = None;
            record.transition(RunPhase::Failed, now.clone());
            record.stop_reason = Some("attention_expired".to_owned());
            self.store.persist_record(&run_id)?;
            self.leases.revoke_run(&run_id);
            self.prompts.remove(&run_id);
            self.append_event(
                &run_id,
                "run.attention_expired",
                json!({ "attentionId": attention_id }),
            )?;
        }
        Ok(())
    }

    fn promote_queued(&mut self) -> HostResult<()> {
        let ceiling = self.config.limits.max_active_runs as usize;
        loop {
            if self.store.count_phase(RunPhase::Running) >= ceiling {
                return Ok(());
            }
            let Some(run_id) = self
                .store
                .records()
                .find(|record| record.phase == RunPhase::Queued)
                .map(|record| record.run_id.clone())
            else {
                return Ok(());
            };
            if self.store.get(&run_id)?.dispatch_blocks_progress() {
                self.halt_for_dispatch(&run_id)?;
                continue;
            }
            if !self.prompts.contains_key(&run_id) {
                self.raise_attention(
                    &run_id,
                    AttentionKind::RecoveryRequired,
                    "prompt_unavailable",
                    "this run needs an explicit prompt before it can run again",
                )?;
                continue;
            }

            let now_ms = self.clock.now_ms();
            let now = self.clock.now_rfc3339();
            let record = self.store.get_mut(&run_id)?;
            record.transition(RunPhase::Running, now);
            record.started_at_ms.get_or_insert(now_ms);
            self.store.persist_record(&run_id)?;
            self.append_event(&run_id, "run.started", json!({}))?;
        }
    }

    fn advance_one(&mut self) -> HostResult<bool> {
        let Some(run_id) = self
            .store
            .records()
            .find(|record| record.phase == RunPhase::Running)
            .map(|record| record.run_id.clone())
        else {
            return Ok(false);
        };

        let now_ms = self.clock.now_ms();
        let record = self.store.get(&run_id)?;

        // Fail closed before anything else. A run whose last dispatch never
        // settled must not take another step: repeating work that may already
        // have happened is worse than stopping and asking.
        if record.dispatch_blocks_progress() {
            self.halt_for_dispatch(&run_id)?;
            return Ok(true);
        }

        let scope = record.scope();
        let round = record.rounds_used.saturating_add(1);
        let max_rounds = record.bounds.max_rounds;
        let max_duration_ms = record.bounds.max_duration_ms;
        let started_at_ms = record.started_at_ms.unwrap_or(now_ms);
        let steering = record.pending_steering.clone();
        let ordinal = record.next_dispatch_ordinal();

        if now_ms.saturating_sub(started_at_ms) >= max_duration_ms {
            self.finish(&run_id, RunPhase::LimitReached, "max_duration")?;
            return Ok(true);
        }

        let Some(prompt) = self.prompts.get(&run_id).cloned() else {
            self.raise_attention(
                &run_id,
                AttentionKind::RecoveryRequired,
                "prompt_unavailable",
                "this run needs an explicit prompt before it can run again",
            )?;
            return Ok(true);
        };

        if self.engine.is_none() {
            return Ok(false);
        }

        // Write-ahead: the record says a dispatch is in flight *before* one is.
        // If this process dies inside the step, the next start finds that fact
        // instead of a record that looks like nothing ever happened.
        let started_at = self.clock.now_rfc3339();
        {
            let record = self.store.get_mut(&run_id)?;
            record.dispatch = Some(DispatchRecord::started(ordinal, round, started_at));
        }
        self.store.persist_record(&run_id)?;
        self.append_event(
            &run_id,
            "run.dispatch_started",
            json!({ "ordinal": ordinal, "round": round }),
        )?;

        let cancel = self.cancel.clone();
        let Some(engine) = self.engine.as_mut() else {
            // Unreachable while the host is single-threaded, but the write-ahead
            // record is already on disk: settle it as nothing-sent rather than
            // leaving a phantom in-flight dispatch for the next start to find.
            let now = self.clock.now_rfc3339();
            if let Some(dispatch) = self.store.get_mut(&run_id)?.dispatch.as_mut() {
                dispatch.settle(DispatchReport::local(), now);
            }
            self.store.persist_record(&run_id)?;
            return Ok(false);
        };
        let result = engine.step(&EngineStep {
            scope: &scope,
            round,
            prompt: &prompt,
            steering: &steering,
            cancel: &cancel,
            dispatch_ordinal: ordinal,
        });

        let now = self.clock.now_rfc3339();
        {
            let record = self.store.get_mut(&run_id)?;
            if let Some(dispatch) = record.dispatch.as_mut() {
                dispatch.settle(result.dispatch, now.clone());
            }
            record.pending_steering.clear();
            record.rounds_used = round;
            record.updated_at = now;
            record.revision = record.revision.saturating_add(1);
        }
        self.store.persist_record(&run_id)?;

        // Read the disposition back from the record rather than from the
        // report: settling downgrades a report whose references are unusable,
        // and the durable value is the one that governs.
        let settlement = self
            .store
            .get(&run_id)?
            .dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.settled.clone());
        let disposition = settlement
            .as_ref()
            .map_or(DispatchDisposition::Indeterminate, |settled| {
                settled.disposition
            });
        self.append_event(
            &run_id,
            "run.dispatch_settled",
            json!({
                "ordinal": ordinal,
                "disposition": disposition.label(),
                "attempt": settlement.as_ref().and_then(|settled| settled.attempt.clone()),
                "receipt": settlement.as_ref().and_then(|settled| settled.receipt.clone()),
            }),
        )?;

        if !disposition.may_advance() {
            self.halt_for_dispatch(&run_id)?;
            return Ok(true);
        }

        match result.outcome {
            EngineOutcome::Progress { update } => {
                self.append_event(&run_id, "run.progress", update)?;
                if round >= max_rounds {
                    self.finish(&run_id, RunPhase::LimitReached, "max_rounds")?;
                }
            }
            EngineOutcome::NeedsAttention {
                attention,
                reason_code,
                detail,
            } => {
                self.raise_attention(&run_id, attention, &reason_code, &detail)?;
            }
            EngineOutcome::Completed {
                changed_files,
                diff,
                fingerprint,
            } => {
                self.complete(&run_id, changed_files, &diff, &fingerprint)?;
            }
            EngineOutcome::Failed {
                reason_code,
                detail,
            } => {
                let (detail, _) = self.redaction.scrub_bounded(&detail, 512);
                self.append_event(
                    &run_id,
                    "run.failed",
                    json!({ "reasonCode": reason_code, "detail": detail }),
                )?;
                self.finish(&run_id, RunPhase::Failed, &reason_code)?;
            }
        }
        Ok(true)
    }

    fn complete(
        &mut self,
        run_id: &str,
        changed_files: Vec<crate::engine::EngineChangedFile>,
        diff: &str,
        engine_fingerprint: &str,
    ) -> HostResult<()> {
        if engine_fingerprint.trim().is_empty() {
            self.append_event(
                run_id,
                "run.failed",
                json!({ "reasonCode": "completion_missing_fingerprint" }),
            )?;
            return self.finish(run_id, RunPhase::Failed, "completion_missing_fingerprint");
        }

        let mut records = Vec::with_capacity(changed_files.len());
        for file in changed_files {
            let Some(path) = relative_path(&file.path, MAX_CHANGED_PATH_BYTES) else {
                self.append_event(
                    run_id,
                    "run.failed",
                    json!({ "reasonCode": "completion_path_rejected" }),
                )?;
                return self.finish(run_id, RunPhase::Failed, "completion_path_rejected");
            };
            let (summary, _) = self.redaction.scrub_bounded(&file.summary, 512);
            records.push(ChangedFileRecord { path, summary });
        }
        let (diff, diff_truncated) = self.redaction.scrub_bounded(diff, MAX_REVIEW_DIFF_BYTES);
        let (fingerprint, _) = self.redaction.scrub_bounded(engine_fingerprint, 256);
        let changed_count = records.len();

        {
            let record = self.store.get_mut(run_id)?;
            record.completion = Some(CompletionRecord {
                changed_files: records,
                diff,
                diff_truncated,
                fingerprint,
            });
        }
        self.append_event(
            run_id,
            "run.completed",
            json!({ "changedFiles": changed_count, "diffTruncated": diff_truncated }),
        )?;
        self.finish(run_id, RunPhase::Completed, "completed")
    }

    fn finish(&mut self, run_id: &str, phase: RunPhase, reason: &str) -> HostResult<()> {
        let now = self.clock.now_rfc3339();
        {
            let record = self.store.get_mut(run_id)?;
            record.transition(phase, now);
            record.stop_reason = Some(reason.to_owned());
            record.pending_steering.clear();
        }
        self.store.persist_record(run_id)?;
        self.leases.revoke_run(run_id);
        self.prompts.remove(run_id);
        self.append_event(
            run_id,
            "run.finished",
            json!({ "phase": phase.label(), "reason": reason }),
        )
    }

    /// Halt a run whose dispatch cannot be proven either way.
    fn halt_for_dispatch(&mut self, run_id: &str) -> HostResult<()> {
        self.raise_attention(
            run_id,
            AttentionKind::DispatchUncertain,
            "dispatch_indeterminate",
            "a dispatch could not be proven delivered or undelivered; reconcile it before continuing",
        )
    }

    /// Attach an escalation without changing the run's phase.
    ///
    /// Used during recovery, where the phase the store already chose
    /// (`interrupted`) is the accurate one and the escalation only explains it.
    fn attach_attention(
        &mut self,
        run_id: &str,
        kind: AttentionKind,
        reason_code: &str,
        detail: &str,
    ) -> HostResult<()> {
        let now_ms = self.clock.now_ms();
        let now = self.clock.now_rfc3339();
        let attention = AttentionRecord::raise(
            &self.redaction,
            run_id,
            kind,
            reason_code,
            detail,
            now,
            now_ms,
            self.config.limits.attention_ttl_ms,
        )?;
        let attention_id = attention.attention_id.clone();
        {
            let record = self.store.get_mut(run_id)?;
            record.attention = Some(attention);
            record
                .stop_reason
                .get_or_insert_with(|| reason_code.to_owned());
        }
        self.store.persist_record(run_id)?;
        self.leases.revoke_run(run_id);
        self.append_event(
            run_id,
            "run.needs_attention",
            json!({
                "attentionId": attention_id,
                "kind": kind.label(),
                "reasonCode": reason_code,
            }),
        )
    }

    fn raise_attention(
        &mut self,
        run_id: &str,
        kind: AttentionKind,
        reason_code: &str,
        detail: &str,
    ) -> HostResult<()> {
        let now_ms = self.clock.now_ms();
        let now = self.clock.now_rfc3339();
        let attention = AttentionRecord::raise(
            &self.redaction,
            run_id,
            kind,
            reason_code,
            detail,
            now.clone(),
            now_ms,
            self.config.limits.attention_ttl_ms,
        )?;
        let attention_id = attention.attention_id.clone();
        {
            let record = self.store.get_mut(run_id)?;
            record.attention = Some(attention);
            record.transition(RunPhase::NeedsAttention, now);
            record.stop_reason = Some(reason_code.to_owned());
        }
        self.store.persist_record(run_id)?;
        self.leases.revoke_run(run_id);
        self.append_event(
            run_id,
            "run.needs_attention",
            json!({
                "attentionId": attention_id,
                "kind": kind.label(),
                "reasonCode": reason_code,
            }),
        )
    }

    // -------------------------------------------------------------- shutdown

    /// Stop the host, checkpointing live runs when the stop is graceful.
    ///
    /// A graceful stop leaves live runs `paused`, so the next start finds
    /// resumable work. An immediate stop leaves them live on disk, so the next
    /// start marks them `interrupted` — the difference is deliberately visible.
    pub fn shutdown(&mut self, kind: ShutdownKind) -> HostResult<StopReport> {
        self.state = HostState::Draining;
        if kind == ShutdownKind::Immediate {
            self.cancel.cancel();
        }
        let mut report = StopReport {
            kind,
            paused: Vec::new(),
            left_live: Vec::new(),
        };

        let live: Vec<String> = self
            .store
            .records()
            .filter(|record| matches!(record.phase, RunPhase::Running | RunPhase::Queued))
            .map(|record| record.run_id.clone())
            .collect();

        match kind {
            ShutdownKind::Graceful => {
                let now = self.clock.now_rfc3339();
                for run_id in live {
                    let record = self.store.get_mut(&run_id)?;
                    record.transition(RunPhase::Paused, now.clone());
                    record.stop_reason = Some("host_shutdown".to_owned());
                    self.store.persist_record(&run_id)?;
                    self.append_event(&run_id, "run.paused", json!({ "reason": "host_shutdown" }))?;
                    report.paused.push(run_id);
                }
            }
            ShutdownKind::None | ShutdownKind::Immediate => {
                report.left_live = live;
            }
        }

        self.leases = LeaseBook::new();
        self.state = HostState::Stopped;
        Ok(report)
    }

    // ----------------------------------------------------------------- utils

    fn append_event(&mut self, run_id: &str, kind: &str, detail: Value) -> HostResult<()> {
        let (phase, revision) = {
            let record = self.store.get(run_id)?;
            (record.phase, record.revision)
        };
        let mut update = json!({
            "event": kind,
            "phase": phase.label(),
            "revision": revision,
        });
        let scrubbed = self.redaction.scrub_value(&detail);
        if let (Some(target), Some(source)) = (update.as_object_mut(), scrubbed.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        } else {
            update["detail"] = scrubbed;
        }

        let bound = self.config.limits.max_event_bytes as usize;
        if serde_json::to_vec(&update).map_or(true, |bytes| bytes.len() > bound) {
            update = json!({
                "event": kind,
                "phase": phase.label(),
                "revision": revision,
                "omitted": "event_exceeded_bound",
            });
        }

        let ts = self.clock.now_rfc3339();
        self.store.journal_mut(run_id)?.append(ts, update)?;
        Ok(())
    }
}

/// The refusal used everywhere an unsettled dispatch blocks an operation.
fn indeterminate_dispatch() -> HostError {
    HostError::forbidden(
        "dispatch_indeterminate",
        "this run has a dispatch that was never proven delivered or undelivered; \
         reconcile it with the orchestrator and submit a fresh run",
    )
}

fn capability_for(class: ControlClass) -> &'static str {
    match class {
        ControlClass::Steer => CAP_QUEUE,
        ControlClass::Pause | ControlClass::Cancel => CAP_EXECUTE,
        ControlClass::Resume => CAP_RESUME,
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> HostResult<Value> {
    serde_json::to_value(value).map_err(|_| {
        HostError::internal(
            "projection_unserializable",
            "the projection cannot be encoded",
        )
    })
}

/// Build the configured engine, if any.
pub fn engine_from_config(config: &HostConfig) -> HostResult<Option<Box<dyn RunEngine>>> {
    match &config.engine {
        EngineSelection::Disabled => Ok(None),
        EngineSelection::Fixture { script } => {
            let script = crate::engine::FixtureScript::load(script)?;
            Ok(Some(Box::new(crate::engine::FixtureEngine::new(script))))
        }
    }
}

/// Build a run scope for this host's session and workspace.
pub fn scope_for(config: &HostConfig, run_id: &str) -> RunScope {
    RunScope {
        session_id: config.session_id.clone(),
        workspace: config.workspace_alias(),
        run_id: run_id.to_owned(),
    }
}
