//! The graph state machine.
//!
//! Nothing here owns a thread, opens a socket, reads a clock, or generates
//! randomness. Callers pass `now` in and persist the record out, which is what
//! makes the whole machine replayable and testable without a provider.
//!
//! This is not a second scheduler. Admission is bounded by the slot count the
//! caller obtained from the host's existing orchestration capacity, so the
//! graph can only ever narrow what the host already permits.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::orchestration::types::{OrchError, OrchErrorCode};

use super::authority::{
    derive_attempt_id, ActionAuthority, AttemptState, ProviderAttemptRecord, SendCertainty,
};
use super::ids::{AttemptId, GraphId, LeaseId, WorkId, WorkerId};
use super::spec::{FailurePolicy, IsolationRequirement, WorkCapability, WorkerRole};
use super::state::{
    truncate_text, GrantBinding, GraphLifecycle, LeaseRecord, LeaseState, ReviewVerdict,
    WorkGraphRecord, WorkOutcome, WorkResult, WorkState, MAX_REASON_BYTES,
};

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

fn conflict(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::Conflict, message)
}

fn exhausted(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::CapacityExhausted, message)
}

/// A dispatch the graph is willing to admit right now.
///
/// This is a proposal, not a record: nothing has been written and no child
/// exists until the caller hands it back to [`issue_lease`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchIntent {
    pub graph_id: GraphId,
    pub work_id: WorkId,
    pub worker_id: WorkerId,
    pub attempt_id: AttemptId,
    pub attempt: u32,
    pub role: WorkerRole,
    pub capabilities: BTreeSet<WorkCapability>,
    pub isolation: IsolationRequirement,
    /// True when the caller must attach a live Computer Use grant binding.
    pub requires_computer_use: bool,
    /// The graph epoch this intent was planned under. A later control action
    /// bumps the epoch and invalidates the intent.
    pub epoch: u64,
}

/// Why admission produced nothing. Distinguishes "nothing to do" from
/// "not allowed to do it".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionBlock {
    None,
    NoSlots,
    GraphInFlightCap,
    AttemptBudgetExhausted,
    TokenBudgetExhausted,
    DeadlineExceeded,
    LifecycleStopped,
}

/// The result of one planning pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionPlan {
    pub intents: Vec<DispatchIntent>,
    pub blocked_by: AdmissionBlock,
    pub ready_not_admitted: usize,
}

/// What a caller learned when it probed an uncertain lease.
///
/// Only positive evidence resolves uncertainty. [`DispatchProbe::Unknown`]
/// deliberately resolves nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "probe", deny_unknown_fields)]
pub enum DispatchProbe {
    /// Proven never to have started. Only this verdict makes a resend safe.
    NotStarted,
    /// Proven to be running, with the owner's handle.
    Running { external_ref: String },
    /// Proven to have finished, with the owner's terminal report.
    Settled { outcome: WorkOutcome },
    /// Still unknown.
    Unknown,
}

/// An operator's decision on a reviewed work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Keep the result. The item stays `Succeeded`.
    Keep,
    /// Discard the result. Terminal and truthful: never counted as a success.
    Discard,
}

/// Recompute every derived state from upstream results and quorum gates.
///
/// `Blocked` is derived, not sticky: if an upstream uncertainty is later
/// resolved in favor of success, its dependents become ready again.
pub fn recompute_derived(record: &mut WorkGraphRecord, now: DateTime<Utc>) {
    // Snapshot settled outcomes and verdicts before mutating.
    let states: BTreeMap<WorkId, WorkState> = record
        .work
        .iter()
        .map(|item| (item.work_id.clone(), item.state))
        .collect();
    let verdicts: BTreeMap<WorkId, Option<ReviewVerdict>> = record
        .work
        .iter()
        .map(|item| (item.work_id.clone(), item.verdict))
        .collect();

    let mut next: BTreeMap<WorkId, WorkState> = BTreeMap::new();
    for item in &record.spec.work {
        let current = states
            .get(&item.work_id)
            .copied()
            .unwrap_or(WorkState::Pending);
        if !current.is_derived() {
            continue;
        }
        let mut blocked = false;
        let mut pending = false;
        for dependency in &item.depends_on {
            match states.get(dependency).copied() {
                Some(WorkState::Succeeded) => {}
                Some(state) if state.is_settled() => blocked = true,
                Some(WorkState::Blocked) => blocked = true,
                Some(_) | None => pending = true,
            }
        }
        // A quorum gate is evaluated on reviewer verdicts, not reviewer
        // success: a reviewer that ran to completion and rejected has done its
        // job and still withholds its approval.
        if let Some(gate) = item.quorum.as_ref() {
            let mut approvals = 0u32;
            let mut undecided = 0u32;
            for reviewer in &gate.reviewers {
                match verdicts.get(reviewer).copied().flatten() {
                    Some(ReviewVerdict::Approve) => approvals += 1,
                    Some(ReviewVerdict::Reject) => {}
                    None => {
                        if states
                            .get(reviewer)
                            .copied()
                            .is_some_and(|state| state.is_settled())
                        {
                            // Settled without a verdict never approves.
                        } else {
                            undecided += 1;
                        }
                    }
                }
            }
            if approvals < gate.required_approvals {
                if approvals + undecided < gate.required_approvals {
                    // The gate can no longer be met, whatever happens next.
                    blocked = true;
                } else {
                    pending = true;
                }
            }
        }
        let resolved = if blocked {
            WorkState::Blocked
        } else if pending {
            WorkState::Pending
        } else {
            WorkState::Ready
        };
        if resolved != current {
            next.insert(item.work_id.clone(), resolved);
        }
    }
    for (work_id, state) in next {
        if let Some(item) = record.work_record_mut(&work_id) {
            item.state = state;
            item.updated_at = now;
        }
    }
}

/// Plan what may be admitted right now. Pure: writes nothing.
///
/// `available_slots` is what the host's existing orchestration capacity already
/// granted. The graph narrows it further by its own in-flight cap and budgets.
pub fn plan_admissions(
    record: &WorkGraphRecord,
    available_slots: usize,
    now: DateTime<Utc>,
) -> AdmissionPlan {
    let empty = |blocked_by| AdmissionPlan {
        intents: Vec::new(),
        blocked_by,
        ready_not_admitted: 0,
    };
    if record.lifecycle != GraphLifecycle::Active {
        return empty(AdmissionBlock::LifecycleStopped);
    }
    if now >= record.deadline_at {
        return empty(AdmissionBlock::DeadlineExceeded);
    }
    if record.spec.failure_policy == FailurePolicy::StopGraph
        && record
            .work
            .iter()
            .any(|item| matches!(item.state, WorkState::Failed | WorkState::TimedOut))
    {
        return empty(AdmissionBlock::LifecycleStopped);
    }
    if record.budget.attempts_used >= record.spec.budget.max_total_attempts {
        return empty(AdmissionBlock::AttemptBudgetExhausted);
    }
    if record.budget.tokens_used >= record.spec.budget.max_total_tokens {
        return empty(AdmissionBlock::TokenBudgetExhausted);
    }

    let in_flight = record.in_flight();
    let graph_room = record.spec.budget.max_in_flight.saturating_sub(in_flight);
    let attempt_room = record
        .spec
        .budget
        .max_total_attempts
        .saturating_sub(record.budget.attempts_used) as usize;
    let room = available_slots.min(graph_room).min(attempt_room);

    // Deterministic order: priority descending, then work id ascending.
    let mut candidates: Vec<_> = record
        .work
        .iter()
        .filter(|item| item.state == WorkState::Ready)
        .filter_map(|item| {
            record
                .spec
                .work_item(&item.work_id)
                .map(|spec| (spec, item))
        })
        .filter(|(_, item)| !forbids_same_work_retry(record, &item.work_id))
        .collect();
    candidates.sort_by(|(left, _), (right, _)| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.work_id.as_str().cmp(right.work_id.as_str()))
    });

    let ready_total = candidates.len();
    if room == 0 {
        let blocked_by = if available_slots == 0 {
            AdmissionBlock::NoSlots
        } else if graph_room == 0 {
            AdmissionBlock::GraphInFlightCap
        } else {
            AdmissionBlock::AttemptBudgetExhausted
        };
        return AdmissionPlan {
            intents: Vec::new(),
            blocked_by,
            ready_not_admitted: ready_total,
        };
    }

    let mut intents = Vec::new();
    for (spec, item) in candidates.into_iter().take(room) {
        let Some(worker) = record.spec.worker(&spec.worker_id) else {
            continue;
        };
        let attempt = item.attempts.saturating_add(1);
        let Ok(attempt_id) = derive_attempt_id(&record.graph_id, &spec.work_id, attempt) else {
            continue;
        };
        intents.push(DispatchIntent {
            graph_id: record.graph_id.clone(),
            work_id: spec.work_id.clone(),
            worker_id: spec.worker_id.clone(),
            attempt_id,
            attempt,
            role: worker.role,
            capabilities: spec.capabilities.clone(),
            isolation: spec.isolation,
            requires_computer_use: spec.capabilities.contains(&WorkCapability::ComputerUse),
            epoch: record.epoch,
        });
    }
    let admitted = intents.len();
    AdmissionPlan {
        intents,
        blocked_by: AdmissionBlock::None,
        ready_not_admitted: ready_total.saturating_sub(admitted),
    }
}

/// True when this item's latest finished attempt forbids a same-work retry.
///
/// A send that may have been accepted is never repeated implicitly. Only an
/// explicit, operator-visible new attempt can move past it.
pub fn forbids_same_work_retry(record: &WorkGraphRecord, work_id: &WorkId) -> bool {
    record
        .attempts
        .iter()
        .filter(|attempt| &attempt.work_id == work_id)
        .max_by_key(|attempt| attempt.ordinal)
        .is_some_and(ProviderAttemptRecord::forbids_same_work_retry)
}

/// Write the durable lease for one planned dispatch.
///
/// The lease exists before any child does. Replaying the same intent under the
/// same epoch returns the stored lease instead of minting a second one, so a
/// replayed planning pass cannot authorize a duplicate assignment.
pub fn issue_lease(
    record: &mut WorkGraphRecord,
    intent: &DispatchIntent,
    lease_id: LeaseId,
    authority: &ActionAuthority,
    grant: Option<GrantBinding>,
    now: DateTime<Utc>,
) -> Result<LeaseRecord, OrchError> {
    if intent.graph_id != record.graph_id {
        return Err(invalid("dispatch intent belongs to a different graph"));
    }
    if intent.epoch != record.epoch {
        return Err(OrchError::new(
            OrchErrorCode::StaleVersion,
            "dispatch intent was planned under a superseded graph epoch",
        ));
    }
    if record.lifecycle != GraphLifecycle::Active {
        return Err(conflict("graph is no longer admitting work"));
    }
    let spec = record
        .spec
        .work_item(&intent.work_id)
        .ok_or_else(|| invalid("dispatch intent names undeclared work"))?
        .clone();
    if spec.worker_id != intent.worker_id || spec.capabilities != intent.capabilities {
        return Err(invalid("dispatch intent disagrees with the declared work"));
    }
    let expected_attempt_id = derive_attempt_id(&record.graph_id, &intent.work_id, intent.attempt)?;
    if expected_attempt_id != intent.attempt_id {
        return Err(invalid("dispatch intent attempt identity is not derivable"));
    }
    // Replay: an existing lease for this exact attempt is returned as-is.
    if let Some(existing) = record
        .leases
        .iter()
        .find(|lease| lease.attempt_id == intent.attempt_id)
    {
        if existing.lease_id != lease_id {
            return Err(conflict(
                "this attempt already has a durable lease under another id",
            ));
        }
        return Ok(existing.clone());
    }
    if record.leases.iter().any(|lease| lease.lease_id == lease_id) {
        return Err(conflict("lease id is already present in the ledger"));
    }
    let item = record
        .work_record(&intent.work_id)
        .ok_or_else(|| invalid("dispatch intent names undeclared work"))?;
    if item.state != WorkState::Ready {
        return Err(conflict("work item is not ready for dispatch"));
    }
    if item.attempts.saturating_add(1) != intent.attempt {
        return Err(conflict("dispatch intent attempt ordinal is stale"));
    }
    if forbids_same_work_retry(record, &intent.work_id) {
        return Err(conflict(
            "a prior attempt may have been accepted; an explicit new attempt is required",
        ));
    }
    if record.budget.attempts_used >= record.spec.budget.max_total_attempts {
        return Err(exhausted("graph attempt budget is exhausted"));
    }
    if intent.requires_computer_use && grant.is_none() {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "work requires Computer Use but no grant binding was attached",
        ));
    }
    if !intent.requires_computer_use && grant.is_some() {
        return Err(invalid(
            "a Computer Use grant was attached to work that does not require it",
        ));
    }
    if authority.attempt_id != intent.attempt_id
        || authority.work_id != intent.work_id
        || authority.attempt != intent.attempt
        || authority.graph_id != record.graph_id
    {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "authority is not bound to this dispatch",
        ));
    }
    if !intent.capabilities.is_subset(&authority.capabilities) {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "authority does not cover every capability this dispatch claims",
        ));
    }
    authority.validate()?;
    if let Some(binding) = &grant {
        binding.verify_binding(&lease_id, &intent.attempt_id)?;
    }

    let expires_at = now
        .checked_add_signed(Duration::milliseconds(spec.bounds.max_duration_ms as i64))
        .ok_or_else(|| invalid("lease deadline overflows"))?
        .min(record.deadline_at);
    if expires_at <= now {
        return Err(conflict("graph deadline leaves no room for a new lease"));
    }
    let lease = LeaseRecord {
        lease_id,
        graph_id: record.graph_id.clone(),
        work_id: intent.work_id.clone(),
        worker_id: intent.worker_id.clone(),
        attempt_id: intent.attempt_id.clone(),
        attempt: intent.attempt,
        authority_id: authority.authority_id.clone(),
        session_id: record.session_id,
        workspace: record.workspace.clone(),
        epoch: record.epoch,
        state: LeaseState::Issued,
        external_ref: None,
        grant,
        issued_at: now,
        expires_at,
        acknowledged_at: None,
        settled_at: None,
        uncertain_reason: None,
    };
    lease.validate()?;
    record.leases.push(lease.clone());
    if !record
        .authorities
        .iter()
        .any(|existing| existing.authority_id == authority.authority_id)
    {
        record.authorities.push(authority.clone());
    }
    let budget_attempts = record.budget.attempts_used.saturating_add(1);
    record.budget.attempts_used = budget_attempts;
    if let Some(item) = record.work_record_mut(&intent.work_id) {
        item.state = WorkState::Leased;
        item.attempts = intent.attempt;
        item.current_lease_id = Some(lease.lease_id.clone());
        item.updated_at = now;
    }
    record.updated_at = now;
    Ok(lease)
}

/// Record that one caller won the durable right to spawn this lease's child.
///
/// A lease has exactly one spawn winner, so replaying the request cannot
/// authorize a second child.
pub fn claim_spawn(
    record: &mut WorkGraphRecord,
    lease_id: &LeaseId,
    now: DateTime<Utc>,
) -> Result<(), OrchError> {
    let lease = record
        .lease_mut(lease_id)
        .ok_or_else(|| invalid("lease is not in the ledger"))?;
    match lease.state {
        LeaseState::Issued => {
            lease.state = LeaseState::Claimed;
            record.updated_at = now;
            Ok(())
        }
        LeaseState::Claimed => Ok(()),
        _ => Err(conflict("lease is not available for a spawn claim")),
    }
}

/// The worker acknowledged the lease and reported a handle.
pub fn acknowledge(
    record: &mut WorkGraphRecord,
    lease_id: &LeaseId,
    external_ref: impl Into<String>,
    now: DateTime<Utc>,
) -> Result<(), OrchError> {
    let external_ref = external_ref.into();
    if external_ref.is_empty() || external_ref.len() > 512 || external_ref.contains('\0') {
        return Err(invalid("external reference is invalid"));
    }
    let epoch = record.epoch;
    let lease = record
        .lease_mut(lease_id)
        .ok_or_else(|| invalid("lease is not in the ledger"))?;
    if lease.epoch != epoch {
        return Err(OrchError::new(
            OrchErrorCode::StaleVersion,
            "lease was superseded by a later control epoch",
        ));
    }
    if !matches!(lease.state, LeaseState::Claimed | LeaseState::Acknowledged) {
        return Err(conflict(
            "lease cannot be acknowledged from its current state",
        ));
    }
    lease.state = LeaseState::Acknowledged;
    lease.external_ref = Some(external_ref);
    lease.acknowledged_at = Some(now);
    let work_id = lease.work_id.clone();
    if let Some(item) = record.work_record_mut(&work_id) {
        if item.state == WorkState::Leased {
            item.state = WorkState::Running;
            item.updated_at = now;
        }
    }
    record.updated_at = now;
    Ok(())
}

/// Settle a lease with its owner's terminal report.
pub fn settle(
    record: &mut WorkGraphRecord,
    lease_id: &LeaseId,
    outcome: &WorkOutcome,
    now: DateTime<Utc>,
) -> Result<(), OrchError> {
    outcome.validate()?;
    let epoch = record.epoch;
    let lease = record
        .lease(lease_id)
        .ok_or_else(|| invalid("lease is not in the ledger"))?
        .clone();
    if lease.epoch != epoch && lease.state != LeaseState::Uncertain {
        return Err(OrchError::new(
            OrchErrorCode::StaleVersion,
            "lease was superseded by a later control epoch",
        ));
    }
    if lease.state == LeaseState::Settled {
        return Err(conflict("lease is already settled"));
    }
    if lease.state == LeaseState::Revoked {
        return Err(conflict("lease was revoked"));
    }
    let role = record
        .spec
        .role_of(&lease.work_id)
        .ok_or_else(|| invalid("lease names work with no resolvable role"))?;
    match (role, outcome.verdict) {
        (WorkerRole::Review, None) if outcome.result == WorkResult::Succeeded => {
            return Err(invalid("a completed review must report a verdict"));
        }
        (role, Some(_)) if role != WorkerRole::Review => {
            return Err(invalid("only a review item may report a verdict"));
        }
        _ => {}
    }

    let work_id = lease.work_id.clone();
    if let Some(record_lease) = record.lease_mut(lease_id) {
        record_lease.state = LeaseState::Settled;
        record_lease.settled_at = Some(now);
    }
    let next_state = match outcome.result {
        WorkResult::Succeeded => WorkState::Succeeded,
        WorkResult::Failed => WorkState::Failed,
        WorkResult::Cancelled => WorkState::Cancelled,
        WorkResult::TimedOut => WorkState::TimedOut,
    };
    if let Some(item) = record.work_record_mut(&work_id) {
        item.state = next_state;
        item.verdict = outcome.verdict;
        item.summary = outcome
            .summary
            .as_deref()
            .map(|text| truncate_text(text, super::state::MAX_SUMMARY_BYTES));
        item.evidence = outcome.evidence.clone();
        item.current_lease_id = None;
        item.updated_at = now;
    }
    record.updated_at = now;
    recompute_derived(record, now);
    Ok(())
}

/// Request cancellation of one work item.
///
/// An item that never started is cancelled outright. A live item moves to
/// `Cancelling` and settles only when its owner confirms a terminal outcome, so
/// a cancel never invents a result for a child that may still be running.
pub fn cancel_work(
    record: &mut WorkGraphRecord,
    work_id: &WorkId,
    now: DateTime<Utc>,
) -> Result<WorkState, OrchError> {
    let item = record
        .work_record(work_id)
        .ok_or_else(|| invalid("cancel names undeclared work"))?;
    let state = item.state;
    if state.is_settled() {
        return Ok(state);
    }
    let lease_id = item.current_lease_id.clone();
    let next = match state {
        WorkState::Pending | WorkState::Ready | WorkState::Blocked => WorkState::Cancelled,
        WorkState::Leased | WorkState::Running | WorkState::Cancelling => WorkState::Cancelling,
        // An uncertain dispatch is never cancelled blind: a child that may be
        // running keeps its slot until positive evidence arrives.
        WorkState::DispatchUncertain => WorkState::DispatchUncertain,
        other => other,
    };
    if next == WorkState::Cancelled {
        if let Some(lease_id) = lease_id {
            if let Some(lease) = record.lease_mut(&lease_id) {
                if lease.is_live() {
                    lease.state = LeaseState::Revoked;
                    lease.settled_at = Some(now);
                }
            }
        }
    }
    if let Some(item) = record.work_record_mut(work_id) {
        item.state = next;
        if next == WorkState::Cancelled {
            item.current_lease_id = None;
        }
        item.updated_at = now;
    }
    record.updated_at = now;
    recompute_derived(record, now);
    Ok(next)
}

/// Stop admission for the whole graph and begin winding down live children.
pub fn cancel_graph(
    record: &mut WorkGraphRecord,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<(), OrchError> {
    if record.lifecycle.is_terminal() {
        return Err(conflict("graph already reached a terminal lifecycle"));
    }
    record.lifecycle = GraphLifecycle::Cancelling;
    record.stop_reason = Some(truncate_text(reason, MAX_REASON_BYTES));
    record.epoch = record.epoch.saturating_add(1);
    let work_ids: Vec<WorkId> = record
        .work
        .iter()
        .map(|item| item.work_id.clone())
        .collect();
    for work_id in work_ids {
        cancel_work(record, &work_id, now)?;
    }
    record.updated_at = now;
    Ok(())
}

/// Expire leases whose execution bound has passed.
///
/// A timeout is a truthful terminal state for the work item, and the lease is
/// revoked rather than silently reused.
pub fn sweep_timeouts(record: &mut WorkGraphRecord, now: DateTime<Utc>) -> usize {
    let expired: Vec<LeaseId> = record
        .leases
        .iter()
        .filter(|lease| lease.is_live() && now >= lease.expires_at)
        .map(|lease| lease.lease_id.clone())
        .collect();
    let mut swept = 0usize;
    for lease_id in expired {
        let Some(lease) = record.lease_mut(&lease_id) else {
            continue;
        };
        let acknowledged = lease.state == LeaseState::Acknowledged;
        let work_id = lease.work_id.clone();
        lease.state = LeaseState::Revoked;
        lease.settled_at = Some(now);
        if let Some(item) = record.work_record_mut(&work_id) {
            // A child that acknowledged may still be running when its bound
            // expires; its fate is unknown, not failed.
            item.state = if acknowledged {
                WorkState::DispatchUncertain
            } else {
                WorkState::TimedOut
            };
            item.current_lease_id = None;
            item.last_error = Some("execution bound expired".into());
            item.updated_at = now;
        }
        swept += 1;
    }
    if swept > 0 {
        record.updated_at = now;
        recompute_derived(record, now);
    }
    swept
}

/// Restart recovery.
///
/// A lease that was written but never acknowledged leaves evidence that a child
/// *may* have started; it becomes `Uncertain`. An acknowledged lease carries a
/// handle, can be probed, and is left running. An admitted provider attempt row
/// with no recorded outcome becomes an uncertain accept.
pub fn recover(record: &mut WorkGraphRecord, now: DateTime<Utc>) -> RecoveryReport {
    let mut report = RecoveryReport::default();
    let uncertain: Vec<LeaseId> = record
        .leases
        .iter()
        .filter(|lease| matches!(lease.state, LeaseState::Issued | LeaseState::Claimed))
        .map(|lease| lease.lease_id.clone())
        .collect();
    for lease_id in uncertain {
        let Some(lease) = record.lease_mut(&lease_id) else {
            continue;
        };
        lease.state = LeaseState::Uncertain;
        lease.uncertain_reason = Some("process restarted before acknowledgement".into());
        let work_id = lease.work_id.clone();
        if let Some(item) = record.work_record_mut(&work_id) {
            item.state = WorkState::DispatchUncertain;
            item.updated_at = now;
        }
        report.leases_marked_uncertain += 1;
    }
    let admitted: Vec<AttemptId> = record
        .attempts
        .iter()
        .filter(|attempt| attempt.state == AttemptState::Admitted)
        .map(|attempt| attempt.attempt_id.clone())
        .collect();
    for attempt_id in admitted {
        if let Some(attempt) = record.attempt_mut(&attempt_id) {
            if attempt.recover_uncertain(now).unwrap_or(false) {
                report.attempts_marked_uncertain += 1;
            }
        }
    }
    for item in &mut record.work {
        if item.state == WorkState::Running && item.current_lease_id.is_none() {
            item.state = WorkState::DispatchUncertain;
            item.updated_at = now;
            report.orphaned_work += 1;
        }
    }
    record.updated_at = now;
    recompute_derived(record, now);
    report
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    pub leases_marked_uncertain: usize,
    pub attempts_marked_uncertain: usize,
    pub orphaned_work: usize,
}

/// Resolve an uncertain lease with positive evidence.
///
/// `Unknown` resolves nothing on purpose: guessing here is how a duplicate
/// child gets spawned or a running child gets abandoned.
pub fn reconcile_uncertain(
    record: &mut WorkGraphRecord,
    lease_id: &LeaseId,
    probe: &DispatchProbe,
    now: DateTime<Utc>,
) -> Result<bool, OrchError> {
    let lease = record
        .lease(lease_id)
        .ok_or_else(|| invalid("lease is not in the ledger"))?
        .clone();
    if lease.state != LeaseState::Uncertain {
        return Err(conflict("lease is not uncertain"));
    }
    let work_id = lease.work_id.clone();
    match probe {
        DispatchProbe::Unknown => Ok(false),
        DispatchProbe::NotStarted => {
            if let Some(entry) = record.lease_mut(lease_id) {
                entry.state = LeaseState::Revoked;
                entry.settled_at = Some(now);
                entry.uncertain_reason = Some("proven never started".into());
            }
            if let Some(item) = record.work_record_mut(&work_id) {
                // The attempt counter is deliberately not rewound: the next
                // dispatch is a new attempt with a new identity.
                item.state = WorkState::Ready;
                item.current_lease_id = None;
                item.updated_at = now;
            }
            record.updated_at = now;
            recompute_derived(record, now);
            Ok(true)
        }
        DispatchProbe::Running { external_ref } => {
            if external_ref.is_empty() || external_ref.len() > 512 {
                return Err(invalid("external reference is invalid"));
            }
            if let Some(entry) = record.lease_mut(lease_id) {
                entry.state = LeaseState::Acknowledged;
                entry.external_ref = Some(external_ref.clone());
                entry.acknowledged_at = Some(now);
                entry.uncertain_reason = None;
            }
            if let Some(item) = record.work_record_mut(&work_id) {
                item.state = WorkState::Running;
                item.current_lease_id = Some(lease_id.clone());
                item.updated_at = now;
            }
            record.updated_at = now;
            Ok(true)
        }
        DispatchProbe::Settled { outcome } => {
            settle(record, lease_id, outcome, now)?;
            Ok(true)
        }
    }
}

/// Install a provider attempt row before the host enters the transport.
pub fn record_attempt_admitted(
    record: &mut WorkGraphRecord,
    lease_id: &LeaseId,
    now: DateTime<Utc>,
) -> Result<ProviderAttemptRecord, OrchError> {
    let lease = record
        .lease(lease_id)
        .ok_or_else(|| invalid("lease is not in the ledger"))?
        .clone();
    if !lease.is_live() {
        return Err(conflict("lease is not live"));
    }
    if let Some(existing) = record.attempt(&lease.attempt_id) {
        return Ok(existing.clone());
    }
    let authority = record
        .authority(&lease.authority_id)
        .ok_or_else(|| invalid("lease names an authority that is not in the ledger"))?
        .clone();
    let ordinal = record
        .work_record_mut(&lease.work_id)
        .ok_or_else(|| invalid("lease names undeclared work"))?
        .claim_send_ordinal()?;
    let attempt = ProviderAttemptRecord::admitted(&authority, ordinal, now)?;
    record.attempts.push(attempt.clone());
    record.updated_at = now;
    Ok(attempt)
}

/// Record the transport outcome for one attempt and attribute its usage.
pub fn record_attempt_finished(
    record: &mut WorkGraphRecord,
    attempt_id: &AttemptId,
    certainty: SendCertainty,
    http_status: Option<u16>,
    usage: Option<crate::completion::CompletionUsage>,
    now: DateTime<Utc>,
) -> Result<(), OrchError> {
    let previously_finished = record
        .attempt(attempt_id)
        .map(|attempt| attempt.state == AttemptState::Finished)
        .ok_or_else(|| invalid("attempt is not in the ledger"))?;
    let attempt = record
        .attempt_mut(attempt_id)
        .ok_or_else(|| invalid("attempt is not in the ledger"))?;
    attempt.finish(certainty, http_status, usage.clone(), now)?;
    let work_id = attempt.work_id.clone();
    if !previously_finished {
        if let Some(usage) = usage {
            let total = usage
                .total_tokens
                .max(usage.prompt_tokens.saturating_add(usage.completion_tokens));
            record.budget.tokens_used = record.budget.tokens_used.saturating_add(total);
        }
    }
    if certainty == SendCertainty::UncertainAccept {
        if let Some(item) = record.work_record_mut(&work_id) {
            if item.state.occupies_slot() && item.state != WorkState::DispatchUncertain {
                item.state = WorkState::DispatchUncertain;
                item.updated_at = now;
            }
        }
    }
    record.updated_at = now;
    Ok(())
}

/// Apply an operator's decision to a reviewed work item.
pub fn review_work(
    record: &mut WorkGraphRecord,
    work_id: &WorkId,
    decision: ReviewDecision,
    now: DateTime<Utc>,
) -> Result<WorkState, OrchError> {
    let item = record
        .work_record(work_id)
        .ok_or_else(|| invalid("review names undeclared work"))?;
    if item.state != WorkState::Succeeded && decision == ReviewDecision::Discard {
        // Discarding an item that never succeeded would overstate what the
        // review decided; there is nothing to discard.
        return Err(conflict("only a succeeded work item can be discarded"));
    }
    let next = match decision {
        ReviewDecision::Keep => WorkState::Succeeded,
        ReviewDecision::Discard => WorkState::Discarded,
    };
    if let Some(item) = record.work_record_mut(work_id) {
        item.state = next;
        item.updated_at = now;
    }
    record.updated_at = now;
    recompute_derived(record, now);
    Ok(next)
}

/// Settle the whole-graph lifecycle, truthfully.
///
/// The graph refuses to declare any terminal outcome — including a completed
/// cancellation — while a child's fate is unknown or capacity is still held.
pub fn settle_lifecycle(record: &mut WorkGraphRecord, now: DateTime<Utc>) -> GraphLifecycle {
    if record.lifecycle.is_terminal() {
        return record.lifecycle;
    }
    if record.has_uncertainty() || record.in_flight() > 0 {
        return record.lifecycle;
    }
    let all_settled = record.work.iter().all(|item| item.state.is_settled());
    if !all_settled && record.lifecycle == GraphLifecycle::Active {
        // Items are still pending, ready, or blocked. If nothing can ever run
        // again the graph is finished; otherwise it stays active.
        let any_runnable = record
            .work
            .iter()
            .any(|item| matches!(item.state, WorkState::Pending | WorkState::Ready));
        if any_runnable {
            return record.lifecycle;
        }
    }
    let next = if record.lifecycle == GraphLifecycle::Cancelling {
        GraphLifecycle::Cancelled
    } else if record
        .work
        .iter()
        .all(|item| item.state == WorkState::Succeeded)
    {
        GraphLifecycle::Succeeded
    } else if record
        .work
        .iter()
        .any(|item| matches!(item.state, WorkState::Failed | WorkState::TimedOut))
    {
        GraphLifecycle::Failed
    } else if record
        .work
        .iter()
        .any(|item| item.state == WorkState::Discarded)
        && record
            .work
            .iter()
            .all(|item| matches!(item.state, WorkState::Discarded | WorkState::Succeeded))
    {
        GraphLifecycle::Discarded
    } else {
        GraphLifecycle::Failed
    };
    record.lifecycle = next;
    record.updated_at = now;
    next
}
