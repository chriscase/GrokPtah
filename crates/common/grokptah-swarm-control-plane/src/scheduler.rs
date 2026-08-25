//! The restart-safe swarm scheduler.
//!
//! The scheduler owns one [`SwarmState`] and exposes a two-phase dispatch
//! protocol:
//!
//! 1. [`SwarmController::plan_dispatches`] is a pure projection of the current
//!    state. It proposes work but writes nothing.
//! 2. [`SwarmController::record_dispatch_requested`] writes the durable
//!    dispatch record. Only after that write should the caller actually spawn
//!    the child.
//!
//! Everything about restart safety follows from that order. A crash before the
//! write loses nothing; a crash after it leaves a `Requested` record with no
//! acknowledgement, and [`SwarmController::recover`] turns exactly those into
//! `Uncertain`. An uncertain dispatch is never resent on a guess: it is
//! resolved only by [`SwarmController::reconcile_uncertain`] carrying positive
//! evidence.

use chrono::{DateTime, TimeDelta, Utc};
use std::sync::Arc;

use crate::error::{SwarmError, SwarmErrorCode, SwarmResult};
use crate::ids::{DispatchId, ExternalRefId, TaskId};
use crate::policy::FailurePolicy;
use crate::spec::{ComputerUseLeaseRef, SwarmSpec, TaskKind, TaskSpec, validate_text};
use crate::state::{
    DispatchIntent, DispatchProbe, DispatchRecord, DispatchState, MAX_EVIDENCE_ENTRIES,
    MAX_REASON_BYTES, MAX_SUMMARY_BYTES, ReviewVerdict, SwarmLifecycle, SwarmState, TaskOutcome,
    TaskResult, TaskState, derive_dispatch_id, truncate_text,
};
use crate::store::{DurableSwarmStore, InMemorySwarmStore, LeaseClaim};
use crate::validate::validate_swarm_spec;

fn corrupt(message: impl Into<String>) -> SwarmError {
    SwarmError::corrupt(message)
}

fn corrupt_dispatch(message: impl Into<String>) -> SwarmError {
    corrupt(format!(
        "stored dispatch record is invalid: {}",
        message.into()
    ))
}

/// What a restart recovery pass changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Dispatches that were written but never acknowledged, and are therefore
    /// now uncertain. Each one needs external evidence before its task can be
    /// retried or declared finished.
    pub uncertain: Vec<DispatchId>,
}

/// Result of the durable one-winner spawn claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnClaim {
    pub dispatch: DispatchRecord,
    pub won: bool,
}

impl RecoveryReport {
    /// True when recovery found nothing that needs operator attention.
    pub fn is_clean(&self) -> bool {
        self.uncertain.is_empty()
    }
}

fn validate_loaded_state(state: &SwarmState) -> SwarmResult<()> {
    if state.schema_version != crate::spec::SWARM_SCHEMA_VERSION {
        return Err(corrupt("swarm record schema version is not supported"));
    }
    if state.revision == 0 {
        return Err(corrupt("swarm record revision must be positive"));
    }
    validate_swarm_spec(&state.spec).map_err(|error| {
        corrupt(format!(
            "stored swarm specification is invalid: {}",
            error.message
        ))
    })?;
    if state.updated_at < state.created_at {
        return Err(corrupt("swarm record updated_at precedes created_at"));
    }
    if state
        .stop_reason
        .as_ref()
        .is_some_and(|reason| validate_text(reason, "stop reason", MAX_REASON_BYTES).is_err())
    {
        return Err(corrupt("stored stop reason is invalid"));
    }
    if state.tasks.len() != state.spec.tasks.len() {
        return Err(corrupt(
            "swarm record does not hold one entry per specified task",
        ));
    }
    for task in &state.spec.tasks {
        if state.task(&task.task_id).is_none() {
            return Err(corrupt("swarm record is missing a task entry"));
        }
    }
    if state.total_dispatches > state.spec.budget.max_total_dispatches
        || usize::try_from(state.total_dispatches).ok() != Some(state.dispatches.len())
    {
        return Err(corrupt(
            "the recorded dispatch count is outside budget or does not match stored records",
        ));
    }

    let mut max_attempts = std::collections::BTreeMap::<TaskId, u32>::new();
    let mut seen_leases = std::collections::BTreeSet::new();
    let mut seen_dispatches = std::collections::BTreeSet::new();
    for dispatch in &state.dispatches {
        dispatch
            .dispatch_id
            .validate()
            .map_err(|error| corrupt_dispatch(error.message))?;
        dispatch
            .task_id
            .validate()
            .map_err(|error| corrupt_dispatch(error.message))?;
        dispatch
            .worker_id
            .validate()
            .map_err(|error| corrupt_dispatch(error.message))?;
        if dispatch.attempt == 0 || dispatch.attempt > state.spec.budget.max_total_dispatches {
            return Err(corrupt_dispatch(
                "dispatch attempt is outside the valid range",
            ));
        }
        if !seen_dispatches.insert(dispatch.dispatch_id.clone()) {
            return Err(corrupt(
                "swarm record holds the same dispatch identity twice",
            ));
        }
        let task_spec = state
            .spec
            .task(&dispatch.task_id)
            .ok_or_else(|| corrupt("swarm record holds a dispatch for an unknown task"))?;
        let worker = state
            .spec
            .worker(&task_spec.worker_id)
            .ok_or_else(|| corrupt("stored task names an unknown dispatch worker"))?;
        if dispatch.worker_id != worker.worker_id || dispatch.isolation != worker.isolation {
            return Err(corrupt_dispatch(
                "dispatch worker or isolation does not match its task",
            ));
        }
        let expected =
            derive_dispatch_id(&state.spec.swarm_id, &dispatch.task_id, dispatch.attempt)?;
        if expected != dispatch.dispatch_id {
            return Err(corrupt(
                "stored dispatch identity does not match its swarm, task, and attempt",
            ));
        }
        if let Some(external_ref) = &dispatch.external_ref {
            external_ref
                .validate()
                .map_err(|error| corrupt_dispatch(error.message))?;
        }
        match (&task_spec.computer_use, &dispatch.lease) {
            (Some(requirement), Some(lease)) => lease
                .validate_for(
                    requirement,
                    &state.spec.swarm_id,
                    &dispatch.task_id,
                    &dispatch.dispatch_id,
                    dispatch.requested_at,
                )
                .map_err(|error| corrupt_dispatch(error.message))?,
            (Some(_), None) => {
                return Err(corrupt_dispatch(
                    "Computer Use task dispatch has no bound lease",
                ));
            }
            (None, Some(_)) => {
                return Err(corrupt_dispatch(
                    "non-Computer Use dispatch carries a lease",
                ));
            }
            (None, None) => {}
        }
        if let Some(lease) = &dispatch.lease
            && !seen_leases.insert(lease.lease_id.clone())
        {
            return Err(corrupt(
                "the same Computer Use lease is attached to multiple dispatches",
            ));
        }
        if dispatch.requested_at < state.created_at
            || dispatch
                .acknowledged_at
                .is_some_and(|at| at < dispatch.requested_at)
            || dispatch
                .settled_at
                .is_some_and(|at| at < dispatch.acknowledged_at.unwrap_or(dispatch.requested_at))
        {
            return Err(corrupt_dispatch(
                "dispatch timestamps are not monotonic or precede swarm creation",
            ));
        }
        match dispatch.state {
            DispatchState::Requested | DispatchState::SpawnClaimed => {
                if dispatch.external_ref.is_some()
                    || dispatch.acknowledged_at.is_some()
                    || dispatch.settled_at.is_some()
                    || dispatch.uncertain_reason.is_some()
                {
                    return Err(corrupt_dispatch(
                        "pre-acknowledgement dispatch has terminal fields",
                    ));
                }
            }
            DispatchState::Acknowledged => {
                if dispatch.external_ref.is_none()
                    || dispatch.acknowledged_at.is_none()
                    || dispatch.settled_at.is_some()
                    || dispatch.uncertain_reason.is_some()
                {
                    return Err(corrupt_dispatch(
                        "acknowledged dispatch does not have exactly its acknowledgement fields",
                    ));
                }
            }
            DispatchState::Settled => {
                if dispatch.settled_at.is_none() || dispatch.uncertain_reason.is_some() {
                    return Err(corrupt_dispatch(
                        "settled dispatch does not have a settlement timestamp",
                    ));
                }
            }
            DispatchState::Uncertain => {
                if dispatch.uncertain_reason.as_ref().is_none_or(|reason| {
                    validate_text(reason, "uncertainty reason", MAX_REASON_BYTES).is_err()
                }) || dispatch.settled_at.is_some()
                {
                    return Err(corrupt_dispatch(
                        "uncertain dispatch does not have exactly its uncertainty fields",
                    ));
                }
            }
        }
        max_attempts
            .entry(dispatch.task_id.clone())
            .and_modify(|attempt| *attempt = (*attempt).max(dispatch.attempt))
            .or_insert(dispatch.attempt);
    }

    for task in &state.tasks {
        if task.updated_at < state.created_at
            || task.attempts > state.spec.budget.max_total_dispatches
        {
            return Err(corrupt("stored task timestamp or attempt count is invalid"));
        }
        if task.summary.as_ref().is_some_and(|summary| {
            validate_text(summary, "outcome summary", MAX_SUMMARY_BYTES).is_err()
        }) || task
            .last_error
            .as_ref()
            .is_some_and(|error| validate_text(error, "task error", MAX_REASON_BYTES).is_err())
            || task.evidence.len() > MAX_EVIDENCE_ENTRIES
        {
            return Err(corrupt("stored task output exceeds its declared bounds"));
        }
        for evidence in &task.evidence {
            evidence
                .validate()
                .map_err(|error| corrupt(error.message))?;
        }
        let task_dispatch = task
            .current_dispatch
            .as_ref()
            .map(|dispatch_id| {
                state
                    .dispatch(dispatch_id)
                    .ok_or_else(|| corrupt("task points at a missing dispatch"))
            })
            .transpose()?;
        if let Some(dispatch) = task_dispatch
            && (dispatch.task_id != task.task_id || dispatch.attempt != task.attempts)
        {
            return Err(corrupt(
                "task current_dispatch does not match its task and latest attempt",
            ));
        }
        match task.state {
            TaskState::Dispatching => {
                if task_dispatch.is_none_or(|dispatch| {
                    !matches!(
                        dispatch.state,
                        DispatchState::Requested | DispatchState::SpawnClaimed
                    )
                }) {
                    return Err(corrupt(
                        "dispatching task does not point at a pre-acknowledgement dispatch",
                    ));
                }
            }
            TaskState::Running => {
                if task_dispatch
                    .is_none_or(|dispatch| dispatch.state != DispatchState::Acknowledged)
                {
                    return Err(corrupt(
                        "running task does not point at an acknowledged dispatch",
                    ));
                }
            }
            TaskState::Cancelling => {
                if task_dispatch.is_none_or(|dispatch| {
                    !matches!(
                        dispatch.state,
                        DispatchState::Requested
                            | DispatchState::SpawnClaimed
                            | DispatchState::Acknowledged
                    )
                }) {
                    return Err(corrupt("cancelling task does not point at a live dispatch"));
                }
            }
            TaskState::DispatchUncertain => {
                if task_dispatch.is_none_or(|dispatch| dispatch.state != DispatchState::Uncertain) {
                    return Err(corrupt(
                        "uncertain task does not point at an uncertain dispatch",
                    ));
                }
            }
            _ if task_dispatch.is_some() => {
                return Err(corrupt(
                    "settled or derived task points at a current dispatch",
                ));
            }
            _ => {}
        }
        if let Some(max_attempt) = max_attempts.get(&task.task_id) {
            if *max_attempt != task.attempts {
                return Err(corrupt(
                    "task attempt counter does not match its dispatch history",
                ));
            }
        } else if task.attempts != 0 {
            return Err(corrupt(
                "task has attempts recorded without dispatch history",
            ));
        }
        let kind = state
            .spec
            .task(&task.task_id)
            .ok_or_else(|| corrupt("stored task is not declared by the specification"))?
            .kind;
        if kind != TaskKind::Review && task.verdict.is_some() {
            return Err(corrupt("non-review task has a verdict"));
        }
        if kind == TaskKind::Review && task.state == TaskState::Succeeded && task.verdict.is_none()
        {
            return Err(corrupt("succeeded review task has no verdict"));
        }
        if kind == TaskKind::Review && task.state != TaskState::Succeeded && task.verdict.is_some()
        {
            return Err(corrupt("non-successful review task has a verdict"));
        }
    }
    for dispatch in &state.dispatches {
        if dispatch.state != DispatchState::Settled
            && state
                .task(&dispatch.task_id)
                .and_then(|task| task.current_dispatch.as_ref())
                != Some(&dispatch.dispatch_id)
        {
            return Err(corrupt(
                "live or uncertain dispatch is not the task's current dispatch",
            ));
        }
    }

    let live = state.tasks.iter().any(|task| task.state.occupies_slot());
    if state.lifecycle.is_terminal() && live {
        return Err(corrupt("terminal swarm still has a live or uncertain task"));
    }
    if state.lifecycle == SwarmLifecycle::Succeeded
        && state
            .tasks
            .iter()
            .any(|task| task.state != TaskState::Succeeded)
    {
        return Err(corrupt("succeeded swarm has a non-succeeded task"));
    }
    if matches!(
        state.lifecycle,
        SwarmLifecycle::Cancelling | SwarmLifecycle::Cancelled
    ) && state.tasks.iter().any(|task| {
        matches!(
            task.state,
            TaskState::Pending | TaskState::Ready | TaskState::Blocked
        )
    }) {
        return Err(corrupt("cancelling swarm has an unclassified derived task"));
    }
    Ok(())
}

/// Owns and advances one swarm's durable state.
#[derive(Debug, Clone)]
pub struct SwarmController {
    state: SwarmState,
    store: Arc<dyn DurableSwarmStore>,
}

impl SwarmController {
    /// Validate a specification and start a swarm.
    pub fn new(spec: SwarmSpec, now: DateTime<Utc>) -> SwarmResult<Self> {
        let store = Arc::new(InMemorySwarmStore::default());
        Self::new_with_store(spec, now, store)
    }

    /// Validate a specification, persist it, and start a swarm using `store`.
    ///
    /// Production callers should provide a durable implementation. The
    /// in-memory store used by [`Self::new`] exists only as a deterministic
    /// convenience for local callers and tests.
    pub fn new_with_store(
        spec: SwarmSpec,
        now: DateTime<Utc>,
        store: Arc<dyn DurableSwarmStore>,
    ) -> SwarmResult<Self> {
        validate_swarm_spec(&spec)?;
        let mut controller = Self {
            state: SwarmState::new(spec, now),
            store,
        };
        controller.refresh(now);
        controller.store.create(&controller.state)?;
        Ok(controller)
    }

    /// Reload durable state, re-checking every invariant.
    ///
    /// A record that fails validation is refused rather than resumed: a
    /// hand-edited or corrupted swarm never gets to dispatch children.
    pub fn load(state: SwarmState) -> SwarmResult<Self> {
        let store = Arc::new(InMemorySwarmStore::default());
        validate_loaded_state(&state)?;
        store.create(&state)?;
        Ok(Self { state, store })
    }

    /// Load the latest state from an owner-provided durable store.
    pub fn load_from_store(
        swarm_id: &crate::ids::SwarmId,
        store: Arc<dyn DurableSwarmStore>,
    ) -> SwarmResult<Self> {
        let state = store.load(swarm_id)?;
        if state.spec.swarm_id != *swarm_id {
            return Err(SwarmError::corrupt(
                "durable store returned a record for a different swarm",
            ));
        }
        validate_loaded_state(&state)?;
        Ok(Self { state, store })
    }

    fn validate_now(&self, now: DateTime<Utc>) -> SwarmResult<()> {
        if now < self.state.created_at {
            return Err(SwarmError::invalid(
                "operation time must not precede swarm creation",
            ));
        }
        if now < self.state.updated_at {
            return Err(SwarmError::conflict(
                "operation time moved backwards relative to the durable swarm state",
            ));
        }
        Ok(())
    }

    fn transact<T, F>(&mut self, operation: F) -> SwarmResult<T>
    where
        F: Fn(&mut Self) -> SwarmResult<T>,
    {
        for _ in 0..=1 {
            let latest = self.store.load(&self.state.spec.swarm_id)?;
            validate_loaded_state(&latest)?;
            if latest.revision != self.state.revision {
                self.state = latest;
            }

            let mut next = self.clone();
            let result = operation(&mut next)?;
            validate_loaded_state(&next.state)?;
            if next.state == self.state {
                return Ok(result);
            }

            let lease_claim = next.new_lease_claim(&self.state);
            if let Some(claim) = &lease_claim {
                let dispatch = next.state.dispatch(&claim.dispatch_id).ok_or_else(|| {
                    SwarmError::corrupt("new Computer Use dispatch has no lease payload")
                })?;
                let lease = dispatch.lease.as_ref().ok_or_else(|| {
                    SwarmError::corrupt("new Computer Use dispatch has no lease payload")
                })?;
                self.store.verify_lease(lease, dispatch.requested_at)?;
            }
            match self.store.compare_and_swap(
                &self.state.spec.swarm_id,
                self.state.revision,
                &next.state,
                lease_claim.as_ref(),
            ) {
                Ok(()) => {
                    self.state = next.state;
                    return Ok(result);
                }
                Err(error) if error.code == SwarmErrorCode::Conflict => continue,
                Err(error) => return Err(error),
            }
        }

        Err(SwarmError::conflict(
            "swarm changed concurrently; retry the operation",
        ))
    }

    fn new_lease_claim(&self, previous: &SwarmState) -> Option<LeaseClaim> {
        self.state
            .dispatches
            .iter()
            .filter(|dispatch| previous.dispatch(&dispatch.dispatch_id).is_none())
            .find_map(|dispatch| {
                dispatch.lease.as_ref().map(|lease| {
                    LeaseClaim::from_dispatch(&self.state.spec.swarm_id, dispatch, lease)
                })
            })
    }

    fn record_dispatch_requested_local(
        &mut self,
        intent: &DispatchIntent,
        lease: Option<ComputerUseLeaseRef>,
        now: DateTime<Utc>,
    ) -> SwarmResult<DispatchRecord> {
        self.validate_now(now)?;
        self.record_dispatch_requested_inner(intent, lease, now)
    }

    /// Borrow the latest durable state for persistence or projection.
    pub fn state(&self) -> &SwarmState {
        &self.state
    }

    /// Take ownership of the durable state.
    pub fn into_state(self) -> SwarmState {
        self.state
    }

    /// Borrow the validated specification.
    pub fn spec(&self) -> &SwarmSpec {
        &self.state.spec
    }

    /// Fail every dispatch whose outcome cannot be known after a restart.
    ///
    /// A `Requested` dispatch was written before the spawn and never
    /// acknowledged, so the child may or may not exist. Those become
    /// `Uncertain`. An `Acknowledged` dispatch carries a provider handle and is
    /// left alone: it can be probed, so it is not uncertain.
    pub fn recover(&mut self, now: DateTime<Utc>) -> SwarmResult<RecoveryReport> {
        self.transact(|next| next.recover_inner(now))
    }

    fn recover_inner(&mut self, now: DateTime<Utc>) -> SwarmResult<RecoveryReport> {
        self.validate_now(now)?;
        let mut report = RecoveryReport::default();
        let stale: Vec<DispatchId> = self
            .state
            .dispatches
            .iter()
            .filter(|record| {
                matches!(
                    record.state,
                    DispatchState::Requested | DispatchState::SpawnClaimed
                )
            })
            .map(|record| record.dispatch_id.clone())
            .collect();

        for dispatch_id in stale {
            let task_id = {
                let Some(record) = self.state.dispatch_mut(&dispatch_id) else {
                    continue;
                };
                record.state = DispatchState::Uncertain;
                record.uncertain_reason = Some(
                    "the owning process restarted before the worker acknowledged this dispatch"
                        .to_string(),
                );
                record.task_id.clone()
            };
            if let Some(task) = self.state.task_mut(&task_id) {
                task.state = TaskState::DispatchUncertain;
                task.updated_at = now;
            }
            report.uncertain.push(dispatch_id);
        }

        if !report.is_clean() {
            self.bump(now);
        }
        self.refresh(now);
        Ok(report)
    }

    /// Propose the dispatches admissible right now.
    ///
    /// Pure: it reads state and writes nothing. Ordering is deterministic —
    /// priority descending, then task ID ascending — so a replay after a
    /// restart proposes the same identities in the same order.
    pub fn plan_dispatches(&self, now: DateTime<Utc>) -> Vec<DispatchIntent> {
        if self.state.lifecycle != SwarmLifecycle::Active || !self.budget_allows(now) {
            return Vec::new();
        }
        let Some(capacity) = self.free_slots() else {
            return Vec::new();
        };
        let budget_left = self
            .state
            .spec
            .budget
            .max_total_dispatches
            .saturating_sub(self.state.total_dispatches);
        let admit = capacity.min(budget_left) as usize;
        if admit == 0 {
            return Vec::new();
        }

        let mut ready: Vec<&TaskSpec> = self
            .state
            .spec
            .tasks
            .iter()
            .filter(|task| {
                self.state
                    .task(&task.task_id)
                    .is_some_and(|record| record.state == TaskState::Ready)
            })
            .collect();
        ready.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.task_id.cmp(&b.task_id))
        });

        ready
            .into_iter()
            .take(admit)
            .filter_map(|task| {
                let record = self.state.task(&task.task_id)?;
                let worker = self.state.spec.worker(&task.worker_id)?;
                let attempt = record.attempts.saturating_add(1);
                let dispatch_id =
                    derive_dispatch_id(&self.state.spec.swarm_id, &task.task_id, attempt).ok()?;
                Some(DispatchIntent {
                    dispatch_id,
                    task_id: task.task_id.clone(),
                    worker_id: worker.worker_id.clone(),
                    attempt,
                    provider: worker.provider.clone(),
                    model: worker.model.clone(),
                    role: worker.role,
                    capability_mode: worker.capability_mode,
                    capabilities: worker.capabilities.clone(),
                    isolation: worker.isolation,
                    requires_computer_use: task.requires_computer_use,
                })
            })
            .collect()
    }

    /// Record a dispatch *before* the child is spawned.
    ///
    /// Replay-safe: if the identity is already on record the stored record is
    /// returned unchanged, and no counter moves. Callers must then win
    /// [`Self::claim_dispatch_spawn`] before spawning. A replay after recovery
    /// legitimately returns an `Uncertain` record, which must not be respawned.
    pub fn record_dispatch_requested(
        &mut self,
        intent: &DispatchIntent,
        lease: Option<ComputerUseLeaseRef>,
        now: DateTime<Utc>,
    ) -> SwarmResult<DispatchRecord> {
        self.transact(|next| next.record_dispatch_requested_local(intent, lease.clone(), now))
    }

    fn record_dispatch_requested_inner(
        &mut self,
        intent: &DispatchIntent,
        lease: Option<ComputerUseLeaseRef>,
        now: DateTime<Utc>,
    ) -> SwarmResult<DispatchRecord> {
        if let Some(existing) = self.state.dispatch(&intent.dispatch_id) {
            if existing.task_id != intent.task_id
                || existing.worker_id != intent.worker_id
                || existing.attempt != intent.attempt
                || existing.isolation != intent.isolation
                || existing.lease.as_ref() != lease.as_ref()
            {
                return Err(SwarmError::conflict(
                    "dispatch identity is already bound to a different request",
                ));
            }
            return Ok(existing.clone());
        }

        if self.state.lifecycle != SwarmLifecycle::Active {
            return Err(SwarmError::conflict(
                "swarm is not active and will not admit new dispatches",
            ));
        }
        let task_spec = self
            .state
            .spec
            .task(&intent.task_id)
            .ok_or_else(|| SwarmError::not_found("dispatch names an unknown task"))?
            .clone();
        let worker = self
            .state
            .spec
            .worker(&task_spec.worker_id)
            .ok_or_else(|| SwarmError::not_found("task names an unknown worker"))?
            .clone();
        if worker.worker_id != intent.worker_id {
            return Err(SwarmError::conflict(
                "dispatch names a worker the task is not assigned to",
            ));
        }
        if worker.provider != intent.provider
            || worker.model != intent.model
            || worker.role != intent.role
            || worker.capability_mode != intent.capability_mode
            || worker.capabilities != intent.capabilities
        {
            return Err(SwarmError::conflict(
                "dispatch worker capabilities do not match the recorded worker",
            ));
        }

        let record = self
            .state
            .task(&intent.task_id)
            .ok_or_else(|| SwarmError::not_found("dispatch names an unknown task"))?;
        if record.state != TaskState::Ready {
            return Err(SwarmError::conflict(
                "task is not ready and cannot be dispatched",
            ));
        }
        let expected_attempt = record.attempts.saturating_add(1);
        if intent.attempt != expected_attempt {
            return Err(SwarmError::conflict(
                "dispatch attempt does not follow the task's recorded attempt count",
            ));
        }
        let expected_id =
            derive_dispatch_id(&self.state.spec.swarm_id, &intent.task_id, intent.attempt)?;
        if expected_id != intent.dispatch_id {
            return Err(SwarmError::conflict(
                "dispatch identity is not the content-derived identity for this attempt",
            ));
        }
        if intent.isolation != worker.isolation {
            return Err(SwarmError::conflict(
                "dispatch isolation does not match the worker's required isolation",
            ));
        }
        if intent.requires_computer_use != task_spec.requires_computer_use {
            return Err(SwarmError::conflict(
                "dispatch Computer Use requirement does not match the task specification",
            ));
        }

        if !self.budget_allows(now) {
            return Err(SwarmError::bound("swarm budget is exhausted"));
        }
        match self.free_slots() {
            Some(free) if free > 0 => {}
            _ => {
                return Err(SwarmError::bound(
                    "no admission slot is free under maxInFlight",
                ));
            }
        }

        let lease = self.authorize_computer_use(
            &task_spec,
            &self.state.spec.swarm_id,
            &intent.task_id,
            &intent.dispatch_id,
            lease,
            now,
        )?;

        self.state.dispatches.push(DispatchRecord {
            dispatch_id: intent.dispatch_id.clone(),
            task_id: intent.task_id.clone(),
            worker_id: worker.worker_id.clone(),
            attempt: intent.attempt,
            isolation: worker.isolation,
            lease,
            state: DispatchState::Requested,
            external_ref: None,
            requested_at: now,
            acknowledged_at: None,
            settled_at: None,
            uncertain_reason: None,
        });
        self.state.total_dispatches = self.state.total_dispatches.saturating_add(1);
        if let Some(task) = self.state.task_mut(&intent.task_id) {
            task.state = TaskState::Dispatching;
            task.attempts = intent.attempt;
            task.current_dispatch = Some(intent.dispatch_id.clone());
            task.updated_at = now;
        }
        self.bump(now);

        Ok(self
            .state
            .dispatch(&intent.dispatch_id)
            .expect("record just pushed")
            .clone())
    }

    /// Atomically claim the right to perform the external spawn.
    ///
    /// Only the caller that observes `won == true` may invoke the provider.
    /// Replaying the same claim returns `won == false`; it never authorizes a
    /// second spawn.
    pub fn claim_dispatch_spawn(
        &mut self,
        dispatch_id: &DispatchId,
        now: DateTime<Utc>,
    ) -> SwarmResult<SpawnClaim> {
        self.transact(|next| next.claim_dispatch_spawn_inner(dispatch_id, now))
    }

    fn claim_dispatch_spawn_inner(
        &mut self,
        dispatch_id: &DispatchId,
        now: DateTime<Utc>,
    ) -> SwarmResult<SpawnClaim> {
        self.validate_now(now)?;
        let record = self
            .state
            .dispatch(dispatch_id)
            .ok_or_else(|| SwarmError::not_found("unknown dispatch"))?;
        match record.state {
            DispatchState::Requested => {}
            DispatchState::SpawnClaimed => {
                return Ok(SpawnClaim {
                    dispatch: record.clone(),
                    won: false,
                });
            }
            _ => {
                return Err(SwarmError::conflict(
                    "dispatch cannot be spawn-claimed from its current state",
                ));
            }
        }
        if self.state.lifecycle != SwarmLifecycle::Active {
            return Err(SwarmError::conflict(
                "swarm is not active and will not claim a spawn",
            ));
        }
        let task = self
            .state
            .task(&record.task_id)
            .ok_or_else(|| SwarmError::corrupt("dispatch points to an unknown task"))?;
        if task.state != TaskState::Dispatching
            || task.current_dispatch.as_ref() != Some(dispatch_id)
        {
            return Err(SwarmError::conflict(
                "dispatch is no longer the current dispatch for its task",
            ));
        }
        if let Some(record) = self.state.dispatch_mut(dispatch_id) {
            record.state = DispatchState::SpawnClaimed;
        }
        self.bump(now);
        Ok(SpawnClaim {
            dispatch: self
                .state
                .dispatch(dispatch_id)
                .expect("dispatch just updated")
                .clone(),
            won: true,
        })
    }

    /// Record that the worker accepted a dispatch and is running.
    pub fn record_dispatch_acknowledged(
        &mut self,
        dispatch_id: &DispatchId,
        external_ref: ExternalRefId,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        self.transact(|next| {
            next.record_dispatch_acknowledged_inner(dispatch_id, &external_ref, now)
        })
    }

    fn record_dispatch_acknowledged_inner(
        &mut self,
        dispatch_id: &DispatchId,
        external_ref: &ExternalRefId,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        self.validate_now(now)?;
        external_ref.validate()?;
        let task_id = {
            let record = self
                .state
                .dispatch_mut(dispatch_id)
                .ok_or_else(|| SwarmError::not_found("unknown dispatch"))?;
            match record.state {
                DispatchState::SpawnClaimed => {}
                DispatchState::Acknowledged
                    if record.external_ref.as_ref() == Some(external_ref) =>
                {
                    return Ok(());
                }
                _ => {
                    return Err(SwarmError::conflict(
                        "dispatch cannot be acknowledged from its current state",
                    ));
                }
            }
            record.state = DispatchState::Acknowledged;
            record.external_ref = Some(external_ref.clone());
            record.acknowledged_at = Some(now);
            record.task_id.clone()
        };
        if let Some(task) = self.state.task_mut(&task_id) {
            if task.state != TaskState::Cancelling {
                task.state = TaskState::Running;
            }
            task.updated_at = now;
        }
        self.bump(now);
        Ok(())
    }

    /// Record that a dispatch's fate is unknown.
    ///
    /// Use this when a spawn call fails in a way that cannot distinguish "never
    /// started" from "started and the reply was lost". The task will not be
    /// retried until [`SwarmController::reconcile_uncertain`] supplies
    /// evidence.
    pub fn record_dispatch_uncertain(
        &mut self,
        dispatch_id: &DispatchId,
        reason: impl Into<String>,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        let reason = reason.into();
        self.transact(|next| next.record_dispatch_uncertain_inner(dispatch_id, &reason, now))
    }

    fn record_dispatch_uncertain_inner(
        &mut self,
        dispatch_id: &DispatchId,
        reason: &str,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        self.validate_now(now)?;
        validate_text(reason, "uncertainty reason", MAX_REASON_BYTES)?;
        let task_id = {
            let record = self
                .state
                .dispatch_mut(dispatch_id)
                .ok_or_else(|| SwarmError::not_found("unknown dispatch"))?;
            if record.state == DispatchState::Uncertain {
                return Ok(());
            }
            if record.state == DispatchState::Settled {
                return Err(SwarmError::conflict(
                    "a settled dispatch cannot become uncertain",
                ));
            }
            record.state = DispatchState::Uncertain;
            record.uncertain_reason = Some(reason.to_string());
            record.task_id.clone()
        };
        if let Some(task) = self.state.task_mut(&task_id) {
            task.state = TaskState::DispatchUncertain;
            task.updated_at = now;
        }
        self.bump(now);
        self.refresh(now);
        Ok(())
    }

    /// Resolve an uncertain dispatch with evidence.
    ///
    /// Returns `true` when the uncertainty was resolved. [`DispatchProbe::Unknown`]
    /// returns `false` and changes nothing — the control plane never converts
    /// absence of evidence into permission to resend.
    pub fn reconcile_uncertain(
        &mut self,
        dispatch_id: &DispatchId,
        probe: DispatchProbe,
        now: DateTime<Utc>,
    ) -> SwarmResult<bool> {
        self.transact(|next| next.reconcile_uncertain_inner(dispatch_id, probe.clone(), now))
    }

    fn reconcile_uncertain_inner(
        &mut self,
        dispatch_id: &DispatchId,
        probe: DispatchProbe,
        now: DateTime<Utc>,
    ) -> SwarmResult<bool> {
        self.validate_now(now)?;
        let current = self
            .state
            .dispatch(dispatch_id)
            .ok_or_else(|| SwarmError::not_found("unknown dispatch"))?;
        if current.state != DispatchState::Uncertain {
            return Err(SwarmError::conflict(
                "only an uncertain dispatch can be reconciled",
            ));
        }
        let task_id = current.task_id.clone();

        match probe {
            DispatchProbe::Unknown => Ok(false),
            DispatchProbe::NotStarted => {
                if let Some(record) = self.state.dispatch_mut(dispatch_id) {
                    record.state = DispatchState::Settled;
                    record.settled_at = Some(now);
                    record.uncertain_reason = None;
                }
                if let Some(task) = self.state.task_mut(&task_id) {
                    // The attempt counter already advanced, so the next
                    // dispatch derives a fresh identity rather than colliding
                    // with the attempt that was proven not to have run.
                    task.state = TaskState::Ready;
                    task.current_dispatch = None;
                    task.updated_at = now;
                }
                self.bump(now);
                self.refresh(now);
                Ok(true)
            }
            DispatchProbe::Running { external_ref } => {
                external_ref.validate()?;
                if let Some(record) = self.state.dispatch_mut(dispatch_id) {
                    record.state = DispatchState::Acknowledged;
                    record.external_ref = Some(external_ref);
                    record.acknowledged_at = Some(now);
                    record.uncertain_reason = None;
                }
                if let Some(task) = self.state.task_mut(&task_id) {
                    task.state = TaskState::Running;
                    task.updated_at = now;
                }
                self.bump(now);
                self.refresh(now);
                Ok(true)
            }
            DispatchProbe::Settled { outcome } => {
                self.apply_outcome(dispatch_id, outcome, now)?;
                Ok(true)
            }
        }
    }

    /// Record a worker's terminal report for a live dispatch.
    pub fn record_task_outcome(
        &mut self,
        dispatch_id: &DispatchId,
        outcome: TaskOutcome,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        self.transact(|next| next.record_task_outcome_inner(dispatch_id, outcome.clone(), now))
    }

    fn record_task_outcome_inner(
        &mut self,
        dispatch_id: &DispatchId,
        outcome: TaskOutcome,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        self.validate_now(now)?;
        let record = self
            .state
            .dispatch(dispatch_id)
            .ok_or_else(|| SwarmError::not_found("unknown dispatch"))?;
        match record.state {
            DispatchState::Requested => {
                return Err(SwarmError::conflict(
                    "dispatch must be spawn-claimed before recording an outcome",
                ));
            }
            DispatchState::SpawnClaimed | DispatchState::Acknowledged => {}
            DispatchState::Uncertain => {
                return Err(SwarmError::new(
                    SwarmErrorCode::UncertainDispatch,
                    "this dispatch is uncertain; resolve it with evidence before recording an outcome",
                ));
            }
            DispatchState::Settled => {
                let task = self
                    .state
                    .task(&record.task_id)
                    .ok_or_else(|| SwarmError::corrupt("dispatch points to an unknown task"))?;
                let expected_state = match outcome.result {
                    TaskResult::Succeeded => TaskState::Succeeded,
                    TaskResult::Failed => TaskState::Failed,
                    TaskResult::Cancelled => TaskState::Cancelled,
                };
                if task.state == expected_state
                    && task.verdict == outcome.verdict
                    && task.summary == outcome.summary
                    && task.evidence == outcome.evidence
                {
                    return Ok(());
                }
                return Err(SwarmError::conflict(
                    "dispatch has already settled with a different outcome",
                ));
            }
        }
        self.apply_outcome(dispatch_id, outcome, now)
    }

    /// Cancel one child.
    ///
    /// A task that has not started is cancelled outright. A live task moves to
    /// `Cancelling` and settles when the caller reports its terminal outcome.
    /// An uncertain task is refused: cancelling a child that may still be
    /// running would release capacity the swarm cannot prove is free.
    pub fn cancel_task(&mut self, task_id: &TaskId, now: DateTime<Utc>) -> SwarmResult<()> {
        self.transact(|next| next.cancel_task_inner(task_id, now))
    }

    fn cancel_task_inner(&mut self, task_id: &TaskId, now: DateTime<Utc>) -> SwarmResult<()> {
        self.validate_now(now)?;
        let record = self
            .state
            .task(task_id)
            .ok_or_else(|| SwarmError::not_found("unknown task"))?;
        match record.state {
            TaskState::DispatchUncertain => {
                return Err(SwarmError::new(
                    SwarmErrorCode::UncertainDispatch,
                    "this task's dispatch is uncertain; reconcile it with evidence before cancelling",
                ));
            }
            state if state.is_settled() => {
                return Err(SwarmError::conflict("task has already settled"));
            }
            _ => {}
        }
        let next = if record.state.occupies_slot() {
            TaskState::Cancelling
        } else {
            TaskState::Cancelled
        };
        if let Some(task) = self.state.task_mut(task_id) {
            task.state = next;
            task.updated_at = now;
        }
        self.bump(now);
        self.refresh(now);
        Ok(())
    }

    /// Cancel the whole swarm.
    ///
    /// New dispatch stops immediately. Tasks that never started are cancelled;
    /// live tasks move to `Cancelling` and settle as their outcomes arrive. The
    /// swarm reaches `Cancelled` only once nothing may still be running —
    /// including anything left uncertain.
    pub fn cancel_swarm(
        &mut self,
        reason: impl Into<String>,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        let reason = reason.into();
        self.transact(|next| next.cancel_swarm_inner(&reason, now))
    }

    fn cancel_swarm_inner(&mut self, reason: &str, now: DateTime<Utc>) -> SwarmResult<()> {
        self.validate_now(now)?;
        validate_text(reason, "cancellation reason", MAX_REASON_BYTES)?;
        if self.state.lifecycle.is_terminal() {
            return Err(SwarmError::conflict(
                "swarm has already reached a terminal state",
            ));
        }
        self.state.lifecycle = SwarmLifecycle::Cancelling;
        self.state.stop_reason = Some(reason.to_string());
        for task in &mut self.state.tasks {
            match task.state {
                TaskState::Pending | TaskState::Ready | TaskState::Blocked => {
                    task.state = TaskState::Cancelled;
                    task.updated_at = now;
                }
                TaskState::Dispatching | TaskState::Running => {
                    task.state = TaskState::Cancelling;
                    task.updated_at = now;
                }
                _ => {}
            }
        }
        self.bump(now);
        self.refresh(now);
        Ok(())
    }

    // ── internals ────────────────────────────────────────────────────────

    fn bump(&mut self, now: DateTime<Utc>) {
        self.state.revision = self.state.revision.saturating_add(1);
        self.state.updated_at = now;
    }

    fn free_slots(&self) -> Option<u32> {
        let used = self
            .state
            .tasks
            .iter()
            .filter(|task| task.state.occupies_slot())
            .count();
        let used = u32::try_from(used).ok()?;
        Some(self.state.spec.admission.max_in_flight.saturating_sub(used))
    }

    fn budget_allows(&self, now: DateTime<Utc>) -> bool {
        if self.state.total_dispatches >= self.state.spec.budget.max_total_dispatches {
            return false;
        }
        let Ok(secs) = i64::try_from(self.state.spec.budget.max_wall_clock_secs) else {
            return false;
        };
        let Some(window) = TimeDelta::try_seconds(secs) else {
            return false;
        };
        let Some(deadline) = self.state.created_at.checked_add_signed(window) else {
            return false;
        };
        now < deadline
    }

    /// Fail-closed Computer Use admission.
    fn authorize_computer_use(
        &self,
        task: &TaskSpec,
        swarm_id: &crate::ids::SwarmId,
        task_id: &TaskId,
        dispatch_id: &DispatchId,
        lease: Option<ComputerUseLeaseRef>,
        now: DateTime<Utc>,
    ) -> SwarmResult<Option<ComputerUseLeaseRef>> {
        match (task.requires_computer_use, lease) {
            (false, None) => Ok(None),
            (false, Some(_)) => Err(SwarmError::capability(
                "a task that does not require Computer Use must not carry a lease",
            )),
            (true, None) => Err(SwarmError::capability(
                "this task requires an operator-issued Computer Use lease",
            )),
            (true, Some(lease)) => {
                let requirement = task.computer_use.as_ref().ok_or_else(|| {
                    SwarmError::corrupt(
                        "Computer Use task is missing its validated authority requirement",
                    )
                })?;
                lease.validate_for(requirement, swarm_id, task_id, dispatch_id, now)?;
                Ok(Some(lease))
            }
        }
    }

    fn apply_outcome(
        &mut self,
        dispatch_id: &DispatchId,
        outcome: TaskOutcome,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        outcome.validate()?;
        let task_id = self
            .state
            .dispatch(dispatch_id)
            .ok_or_else(|| SwarmError::not_found("unknown dispatch"))?
            .task_id
            .clone();
        let kind = self
            .state
            .spec
            .task(&task_id)
            .ok_or_else(|| SwarmError::not_found("dispatch names an unknown task"))?
            .kind;

        match (kind, outcome.result, outcome.verdict) {
            (TaskKind::Review, TaskResult::Succeeded, None) => {
                return Err(SwarmError::invalid(
                    "a completed review task must report a verdict",
                ));
            }
            (kind, _, Some(_)) if kind != TaskKind::Review => {
                return Err(SwarmError::invalid(
                    "only a review task may report a verdict",
                ));
            }
            _ => {}
        }

        if let Some(record) = self.state.dispatch_mut(dispatch_id) {
            record.state = DispatchState::Settled;
            record.settled_at = Some(now);
            record.uncertain_reason = None;
        }

        let next = match outcome.result {
            TaskResult::Succeeded => TaskState::Succeeded,
            TaskResult::Failed => TaskState::Failed,
            TaskResult::Cancelled => TaskState::Cancelled,
        };
        if let Some(task) = self.state.task_mut(&task_id) {
            task.state = next;
            task.verdict = outcome.verdict;
            task.summary = outcome.summary.clone();
            task.last_error = match outcome.result {
                TaskResult::Failed => outcome
                    .summary
                    .clone()
                    .map(|summary| truncate_text(&summary, MAX_REASON_BYTES))
                    .or_else(|| Some("task failed without a summary".to_string())),
                _ => None,
            };
            task.evidence = outcome.evidence;
            task.current_dispatch = None;
            task.updated_at = now;
        }
        self.bump(now);

        if next == TaskState::Failed
            && self.state.spec.failure == FailurePolicy::CancelSwarm
            && !self.state.lifecycle.is_terminal()
            && self.state.lifecycle != SwarmLifecycle::Cancelling
        {
            self.cancel_swarm("a task failed under the cancel-swarm failure policy", now)?;
            return Ok(());
        }

        self.refresh(now);
        Ok(())
    }

    /// Recompute derived task states and the swarm lifecycle.
    ///
    /// Runs to a fixpoint so a blocking result cascades all the way down the
    /// graph in one pass. Bounded by the task count, which validation already
    /// caps.
    fn refresh(&mut self, now: DateTime<Utc>) {
        if self.state.lifecycle.is_terminal() {
            return;
        }
        let cancelling = self.state.lifecycle == SwarmLifecycle::Cancelling;

        for _ in 0..=self.state.tasks.len() {
            let mut changed = false;
            for index in 0..self.state.spec.tasks.len() {
                let task_id = self.state.spec.tasks[index].task_id.clone();
                let Some(current) = self.state.task(&task_id).map(|record| record.state) else {
                    continue;
                };
                if !current.is_derived() {
                    continue;
                }
                let next = if cancelling {
                    TaskState::Cancelled
                } else {
                    self.derive_state(&task_id)
                };
                if next != current {
                    if let Some(task) = self.state.task_mut(&task_id) {
                        task.state = next;
                        task.updated_at = now;
                    }
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        self.refresh_lifecycle(now);
    }

    /// Derive one task's state from its dependencies and review gate.
    fn derive_state(&self, task_id: &TaskId) -> TaskState {
        let Some(spec) = self.state.spec.task(task_id) else {
            return TaskState::Blocked;
        };

        let mut all_succeeded = true;
        for dependency in &spec.dependencies {
            let Some(record) = self.state.task(dependency) else {
                return TaskState::Blocked;
            };
            match record.state {
                TaskState::Succeeded => {}
                // An upstream result that can never become success blocks this
                // node. Uncertainty blocks too: the control plane will not
                // build on a result it cannot prove.
                TaskState::Failed
                | TaskState::Blocked
                | TaskState::Cancelled
                | TaskState::Cancelling
                | TaskState::DispatchUncertain => return TaskState::Blocked,
                _ => all_succeeded = false,
            }
        }
        if !all_succeeded {
            return TaskState::Pending;
        }

        let Some(gate) = &spec.review_gate else {
            return TaskState::Ready;
        };
        let Ok(reviewer_count) = u32::try_from(gate.reviewers.len()) else {
            return TaskState::Blocked;
        };
        let required = gate.quorum.required_approvals(reviewer_count);
        let approvals = gate
            .reviewers
            .iter()
            .filter(|reviewer| {
                self.state
                    .task(reviewer)
                    .is_some_and(|record| record.verdict == Some(ReviewVerdict::Approve))
            })
            .count();
        let approvals = u32::try_from(approvals).unwrap_or(0);
        // Every reviewer is also a dependency, so all of them have already
        // succeeded here. The approval count can no longer improve.
        if approvals >= required {
            TaskState::Ready
        } else {
            TaskState::Blocked
        }
    }

    fn refresh_lifecycle(&mut self, now: DateTime<Utc>) {
        let live = self
            .state
            .tasks
            .iter()
            .any(|task| task.state.occupies_slot());

        if self.state.lifecycle == SwarmLifecycle::Cancelling {
            if !live {
                self.state.lifecycle = SwarmLifecycle::Cancelled;
                self.state.updated_at = now;
            }
            return;
        }

        let admittable = self.budget_allows(now)
            && self
                .state
                .tasks
                .iter()
                .any(|task| task.state == TaskState::Ready);
        if live || admittable {
            return;
        }

        if self
            .state
            .tasks
            .iter()
            .all(|task| task.state == TaskState::Succeeded)
        {
            self.state.lifecycle = SwarmLifecycle::Succeeded;
        } else {
            self.state.lifecycle = SwarmLifecycle::Failed;
            if self.state.stop_reason.is_none() {
                self.state.stop_reason = Some(
                    "the swarm can make no further progress and not every task succeeded"
                        .to_string(),
                );
            }
        }
        self.state.updated_at = now;
    }
}
