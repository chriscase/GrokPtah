//! Durable admission primitives for long-running agent work.
//!
//! This module owns the three durability objects that stand between an
//! accepted control-plane request and a live model turn:
//!
//! * [`AcceptanceIntent`] — the sealed, bounded, private record of *exactly*
//!   what was accepted. It is the only durable source of a queued prompt, so
//!   restart recovery never has to reconstruct execution input from a
//!   receipt, a journal, or an in-memory queue.
//! * [`AttemptLease`] — a compare-and-swap lease that names the single
//!   attempt authorized to dispatch a run. Dispatch without a held lease is
//!   not possible; a stale or wrong-owner heartbeat cannot renew one.
//! * [`LiveWorker`] — the authoritative in-memory registry entry for a
//!   dispatched attempt, holding the nested join handles and the cancel
//!   token, plus the exactly-once settlement latch that gates terminalization
//!   and capacity release.
//!
//! # Crash-safe cuts
//!
//! Admission is cut into steps that are each individually crash-safe. After a
//! crash at any cut, restart recovery reaches exactly one of "never ran" or
//! "runs exactly once", and never "ran twice":
//!
//! | Cut | Durable state after the crash | Recovery |
//! |-----|-------------------------------|----------|
//! | C0 | nothing written | request never happened; a retry is a fresh admission |
//! | C1 | idempotency claim `pending` | claim is failed on open; the request can never later execute |
//! | C2 | claim `pending` + sealed intent | intent is tombstoned (no `complete` receipt); never executes |
//! | C3 | claim `pending` + intent + `Queued` run | run is tombstoned as `admission_lost`; never executes |
//! | C4 | receipt `complete` + intent + `Queued` run | re-admitted from the intent and executed exactly once |
//! | C5 | C4 + attempt lease held | lease is expired/stolen by the next attempt; still exactly once |
//! | C6 | C5 + `Running` run | run is terminalized `Interrupted`; model work never resumes implicitly |
//! | C7 | terminal run, intent still present | intent is reclaimed as garbage; nothing re-executes |
//!
//! The rule that makes the table hold is: **the durable input is written
//! before the receipt is completed, and is never removed before the run is
//! terminal.** A receipt that says "accepted" therefore always has a durable
//! input behind it, and an input without a completed receipt is always
//! garbage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::types::{
    hash_payload, safe_id_filename, OrchError, OrchErrorCode, RunBounds, RunExecutionMode,
};

/// Version of the [`AcceptanceIntent`] seal. A record produced by a different
/// version is rejected rather than reinterpreted.
pub const ACCEPTANCE_INTENT_VERSION: u32 = 1;

/// Version of the [`AttemptLease`] seal.
pub const ATTEMPT_LEASE_VERSION: u32 = 1;

/// Hard ceiling on a sealed prompt, independent of per-run bounds. Bounds are
/// caller-influenced (under a server ceiling); this is not.
pub const MAX_INTENT_PROMPT_BYTES: usize = 1_000_000;

/// Hard ceiling on every short identity/revision field in a sealed record.
pub const MAX_INTENT_FIELD_BYTES: usize = 4 * 1024;

/// Default lifetime of an attempt lease. A holder that stops heartbeating for
/// longer than this is reapable by the next attempt.
pub const DEFAULT_ATTEMPT_LEASE_TTL_MS: u64 = 60_000;

/// Bounded budget for aborting and awaiting one live worker or supervisor.
pub const DEFAULT_TEARDOWN_BUDGET: Duration = Duration::from_secs(5);

/// Bounds as sealed into an [`AcceptanceIntent`].
///
/// This is deliberately a distinct type from [`RunBounds`] so the sealed copy
/// can deny unknown fields without changing how the general-purpose run record
/// evolves. Tampering that adds a field, renames one, or drops one is a parse
/// failure rather than a silently defaulted bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SealedBounds {
    pub max_prompt_bytes: usize,
    pub max_rounds: u32,
    pub max_duration_ms: u64,
}

impl From<&RunBounds> for SealedBounds {
    fn from(value: &RunBounds) -> Self {
        Self {
            max_prompt_bytes: value.max_prompt_bytes,
            max_rounds: value.max_rounds,
            max_duration_ms: value.max_duration_ms,
        }
    }
}

impl From<SealedBounds> for RunBounds {
    fn from(value: SealedBounds) -> Self {
        Self {
            max_prompt_bytes: value.max_prompt_bytes,
            max_rounds: value.max_rounds,
            max_duration_ms: value.max_duration_ms,
        }
    }
}

impl SealedBounds {
    fn validate(&self) -> Result<(), OrchError> {
        RunBounds::from(*self).validate()
    }
}

/// The sealed, bounded, private record of one accepted unit of work.
///
/// Every execution-relevant field is covered by [`AcceptanceIntent::digest`].
/// Loading recomputes the digest and fails closed on any mismatch, so a
/// parseable tamper (a swapped prompt, a widened bound, a redirected
/// workspace, a re-pointed session, a forged request identity) cannot execute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptanceIntent {
    pub intent_version: u32,
    /// Work identity: the run this input will execute as.
    pub run_id: String,
    /// Request identity: the idempotency key that admitted this work.
    pub request_id: String,
    /// Request identity: hash of the exact accepted request payload.
    pub payload_hash: String,
    /// Control tool that admitted the work (part of the idempotency key).
    pub tool: String,
    pub session_id: Uuid,
    /// Session revision observed at acceptance.
    pub session_revision: String,
    /// Canonical claimed workspace.
    pub workspace: String,
    /// Workspace revision observed at acceptance.
    pub workspace_revision: String,
    pub agent_id: Option<String>,
    /// Agent continuation revision observed at acceptance.
    pub agent_revision: u64,
    /// Execution spec revision (bridge contract the input was accepted under).
    pub spec_revision: String,
    /// The full, bounded, private execution input. Never public, never
    /// journaled, never included in a receipt.
    pub prompt: String,
    pub bounds: SealedBounds,
    pub execution_mode: RunExecutionMode,
    pub allow_queue: bool,
    pub retry_of: Option<String>,
    pub parent_run_id: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Versioned integrity digest over every field above.
    pub digest: String,
}

impl AcceptanceIntent {
    /// Compute the seal over every execution-relevant field.
    ///
    /// The prompt enters by content hash and byte length so the digest input
    /// stays bounded while still covering the prompt exactly.
    pub fn digest_for(&self) -> String {
        hash_payload(&serde_json::json!({
            "intentVersion": self.intent_version,
            "runId": self.run_id,
            "requestId": self.request_id,
            "payloadHash": self.payload_hash,
            "tool": self.tool,
            "sessionId": self.session_id,
            "sessionRevision": self.session_revision,
            "workspace": self.workspace,
            "workspaceRevision": self.workspace_revision,
            "agentId": self.agent_id,
            "agentRevision": self.agent_revision,
            "specRevision": self.spec_revision,
            "promptSha256": hash_payload(&serde_json::Value::String(self.prompt.clone())),
            "promptBytes": self.prompt.len(),
            "bounds": {
                "maxPromptBytes": self.bounds.max_prompt_bytes,
                "maxRounds": self.bounds.max_rounds,
                "maxDurationMs": self.bounds.max_duration_ms,
            },
            "executionMode": self.execution_mode,
            "allowQueue": self.allow_queue,
            "retryOf": self.retry_of,
            "parentRunId": self.parent_run_id,
            "createdAt": self.created_at.to_rfc3339(),
        }))
    }

    /// Stamp the seal. Call exactly once, at acceptance.
    pub fn seal(mut self) -> Self {
        self.digest = self.digest_for();
        self
    }

    /// Fail closed on anything that is not exactly what was accepted.
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.intent_version != ACCEPTANCE_INTENT_VERSION {
            return Err(OrchError::new(
                OrchErrorCode::Unsupported,
                format!(
                    "acceptance intent version {} is not supported",
                    self.intent_version
                ),
            ));
        }
        validate_identity(&self.run_id, "run_id")?;
        validate_identity(&self.request_id, "request_id")?;
        validate_identity(&self.tool, "tool")?;
        if let Some(agent_id) = self.agent_id.as_deref() {
            validate_identity(agent_id, "agent_id")?;
        }
        if let Some(retry_of) = self.retry_of.as_deref() {
            validate_identity(retry_of, "retry_of")?;
        }
        if let Some(parent) = self.parent_run_id.as_deref() {
            validate_identity(parent, "parent_run_id")?;
        }
        validate_hex_digest(&self.payload_hash, "payload_hash")?;
        validate_bounded(&self.workspace, "workspace")?;
        validate_bounded_allow_empty(&self.session_revision, "session_revision")?;
        validate_bounded_allow_empty(&self.workspace_revision, "workspace_revision")?;
        validate_bounded(&self.spec_revision, "spec_revision")?;
        self.bounds.validate()?;
        if self.prompt.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "acceptance intent prompt is empty",
            ));
        }
        if self.prompt.len() > MAX_INTENT_PROMPT_BYTES {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "acceptance intent prompt exceeds the hard ceiling",
            ));
        }
        if self.prompt.len() > self.bounds.max_prompt_bytes {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "acceptance intent prompt exceeds its sealed bound",
            ));
        }
        validate_hex_digest(&self.digest, "digest")?;
        if self.digest != self.digest_for() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "acceptance intent digest does not match its sealed fields",
            ));
        }
        Ok(())
    }

    /// The bounds this input executes under, as a run record carries them.
    pub fn run_bounds(&self) -> RunBounds {
        RunBounds::from(self.bounds)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptLeaseState {
    Held,
    Released,
}

/// A compare-and-swap lease naming the single attempt authorized to dispatch
/// one run. Acquisition bumps [`AttemptLease::attempt`] and mints a fresh
/// [`AttemptLease::attempt_id`], so a stale holder can never be mistaken for
/// the current one even after it comes back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptLease {
    pub lease_version: u32,
    pub run_id: String,
    /// Monotonic attempt number. Never reused, never decreases.
    pub attempt: u64,
    /// Identity of this attempt. Renew/release require an exact match.
    pub attempt_id: String,
    /// Identity of the process instance holding the lease.
    pub owner_id: String,
    pub session_id: Uuid,
    /// The sealed input this attempt is authorized to execute.
    pub intent_digest: String,
    pub acquired_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    pub lease_ttl_ms: u64,
    pub state: AttemptLeaseState,
    pub digest: String,
}

impl AttemptLease {
    pub fn digest_for(&self) -> String {
        hash_payload(&serde_json::json!({
            "leaseVersion": self.lease_version,
            "runId": self.run_id,
            "attempt": self.attempt,
            "attemptId": self.attempt_id,
            "ownerId": self.owner_id,
            "sessionId": self.session_id,
            "intentDigest": self.intent_digest,
            "acquiredAt": self.acquired_at.to_rfc3339(),
            "heartbeatAt": self.heartbeat_at.to_rfc3339(),
            "leaseTtlMs": self.lease_ttl_ms,
            "state": self.state,
        }))
    }

    pub fn seal(mut self) -> Self {
        self.digest = self.digest_for();
        self
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.lease_version != ATTEMPT_LEASE_VERSION {
            return Err(OrchError::new(
                OrchErrorCode::Unsupported,
                format!(
                    "attempt lease version {} is not supported",
                    self.lease_version
                ),
            ));
        }
        validate_identity(&self.run_id, "run_id")?;
        validate_identity(&self.attempt_id, "attempt_id")?;
        validate_identity(&self.owner_id, "owner_id")?;
        validate_hex_digest(&self.intent_digest, "intent_digest")?;
        if self.lease_ttl_ms == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "attempt lease ttl must be > 0",
            ));
        }
        validate_hex_digest(&self.digest, "digest")?;
        if self.digest != self.digest_for() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "attempt lease digest does not match its sealed fields",
            ));
        }
        Ok(())
    }

    /// A held lease whose holder stopped heartbeating is reapable. Expiry is
    /// evaluated against the durable heartbeat, never against process memory.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        if self.state != AttemptLeaseState::Held {
            return true;
        }
        let ttl = chrono::Duration::milliseconds(self.lease_ttl_ms.min(i64::MAX as u64) as i64);
        now.signed_duration_since(self.heartbeat_at) > ttl
    }

    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.state == AttemptLeaseState::Held && !self.is_expired(now)
    }
}

fn validate_identity(value: &str, field: &str) -> Result<(), OrchError> {
    validate_bounded(value, field)?;
    safe_id_filename(value).map(|_| ()).map_err(|error| {
        OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{field} is invalid: {}", error.message),
        )
    })
}

fn validate_bounded(value: &str, field: &str) -> Result<(), OrchError> {
    if value.is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{field} must not be empty"),
        ));
    }
    validate_bounded_allow_empty(value, field)
}

fn validate_bounded_allow_empty(value: &str, field: &str) -> Result<(), OrchError> {
    if value.len() > MAX_INTENT_FIELD_BYTES || value.contains('\0') {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{field} is out of range"),
        ));
    }
    Ok(())
}

fn validate_hex_digest(value: &str, field: &str) -> Result<(), OrchError> {
    if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{field} is not a sha-256 digest"),
        ));
    }
    Ok(())
}

/// Observable liveness of one dispatched worker future.
///
/// `finished` is set from a guard held *inside* the worker future, so it flips
/// when the future actually completes or is dropped — including when it is
/// aborted mid-await. Ledger state can be written by anyone; this is the only
/// signal that proves the work itself is gone.
#[derive(Debug, Default)]
pub struct WorkerLiveness {
    started: AtomicBool,
    finished: AtomicBool,
}

impl WorkerLiveness {
    pub fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn mark_started(&self) {
        self.started.store(true, Ordering::Release);
    }

    /// The worker future can no longer execute.
    ///
    /// True either because the future ended — its guard dropped, which covers
    /// completion, cancellation, and abort mid-await — or because it was
    /// cancelled before it was ever polled, so its body never ran and never
    /// will. Both are safe; a future that has started and not finished is not.
    pub fn quiescent(&self) -> bool {
        self.finished() || !self.started()
    }
}

/// Drop guard that records the end of a worker future's life.
pub struct WorkerLivenessGuard {
    liveness: Arc<WorkerLiveness>,
}

impl WorkerLivenessGuard {
    pub fn new(liveness: Arc<WorkerLiveness>) -> Self {
        liveness.mark_started();
        Self { liveness }
    }
}

impl Drop for WorkerLivenessGuard {
    fn drop(&mut self) {
        self.liveness.finished.store(true, Ordering::Release);
    }
}

/// Authoritative registry entry for one dispatched attempt.
///
/// The entry owns the cancel token and abort handles for the *nested* tasks
/// (the model worker and the journal aggregator) plus the join handle of the
/// outer supervisor. Nothing may release this run's capacity or promote
/// another attempt until every one of them has been aborted and bounded-awaited.
pub struct LiveWorker {
    pub run_id: String,
    pub attempt_id: String,
    pub attempt: u64,
    pub session_id: Uuid,
    pub cancel: CancellationToken,
    pub liveness: Arc<WorkerLiveness>,
    worker_abort: parking_lot::Mutex<Option<AbortHandle>>,
    aggregator_abort: parking_lot::Mutex<Option<AbortHandle>>,
    supervisor: parking_lot::Mutex<Option<JoinHandle<()>>>,
    settled: AtomicBool,
}

/// What a bounded teardown actually achieved. `WorkerEscaped` is the honest
/// outcome when a backend ignored cancellation past the budget: capacity is
/// still held, because releasing it would let a second attempt run beside a
/// future that can still execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationOutcome {
    /// Every nested future is confirmed gone and the supervisor has joined.
    Terminated,
    /// The supervisor did not join within the budget even after being aborted.
    SupervisorEscaped,
    /// The worker future was still live after abort + bounded await.
    WorkerEscaped,
}

impl TerminationOutcome {
    /// Only a fully confirmed teardown may release capacity or promote
    /// another attempt.
    pub fn may_release_capacity(self) -> bool {
        matches!(self, Self::Terminated)
    }
}

impl LiveWorker {
    pub fn new(
        run_id: String,
        attempt_id: String,
        attempt: u64,
        session_id: Uuid,
        cancel: CancellationToken,
        liveness: Arc<WorkerLiveness>,
    ) -> Self {
        Self {
            run_id,
            attempt_id,
            attempt,
            session_id,
            cancel,
            liveness,
            worker_abort: parking_lot::Mutex::new(None),
            aggregator_abort: parking_lot::Mutex::new(None),
            supervisor: parking_lot::Mutex::new(None),
            settled: AtomicBool::new(false),
        }
    }

    pub fn attach_worker(&self, abort: AbortHandle) {
        *self.worker_abort.lock() = Some(abort);
    }

    pub fn attach_aggregator(&self, abort: AbortHandle) {
        *self.aggregator_abort.lock() = Some(abort);
    }

    pub fn attach_supervisor(&self, handle: JoinHandle<()>) {
        *self.supervisor.lock() = Some(handle);
    }

    /// Abort the nested worker and aggregator without awaiting them.
    ///
    /// Used by teardown paths that cannot await (notably `Drop`). Callers that
    /// can await must use [`LiveWorker::terminate`] instead, because an abort
    /// alone does not prove the future is gone.
    pub fn abort_nested(&self) {
        if let Some(abort) = self.worker_abort.lock().take() {
            abort.abort();
        }
        if let Some(abort) = self.aggregator_abort.lock().take() {
            abort.abort();
        }
    }

    /// Win the single settlement right for this attempt.
    ///
    /// Exactly one caller ever observes `true`, no matter how many paths race
    /// to reap the run (normal completion, deadline, explicit cancel, reaper,
    /// panic unwind, abort, process shutdown). The winner — and only the
    /// winner — terminalizes the run, releases its durable attempt lease,
    /// drops its durable input, deregisters it, and releases host capacity.
    pub fn settle_once(&self) -> bool {
        self.settled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn is_settled(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }

    /// Cancel, abort, and bounded-await every future this attempt owns.
    ///
    /// Order matters: cooperative cancellation first, then abort of the nested
    /// worker and aggregator, then a bounded join of the supervisor (which is
    /// itself awaiting the worker), then a bounded abort-join of the
    /// supervisor if it overran. The worker's own liveness guard is the proof
    /// of termination; the supervisor joining is not sufficient on its own.
    pub async fn terminate(&self, budget: Duration) -> TerminationOutcome {
        self.cancel.cancel();
        if let Some(abort) = self.worker_abort.lock().take() {
            abort.abort();
        }
        if let Some(abort) = self.aggregator_abort.lock().take() {
            abort.abort();
        }

        let supervisor = self.supervisor.lock().take();
        let mut supervisor_joined = true;
        if let Some(mut handle) = supervisor {
            if tokio::time::timeout(budget, &mut handle).await.is_err() {
                handle.abort();
                supervisor_joined = tokio::time::timeout(budget, &mut handle).await.is_ok();
            }
        }

        if !await_worker_finished(&self.liveness, budget).await {
            return TerminationOutcome::WorkerEscaped;
        }
        if !supervisor_joined {
            return TerminationOutcome::SupervisorEscaped;
        }
        TerminationOutcome::Terminated
    }
}

/// Bounded wait for the worker future's liveness guard to drop.
///
/// A worker that never yields cannot be aborted by tokio, so this is polled
/// rather than awaited on a handle: the honest answer after the budget is
/// "still live", not "assumed gone".
async fn await_worker_finished(liveness: &Arc<WorkerLiveness>, budget: Duration) -> bool {
    if liveness.quiescent() {
        return true;
    }
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if liveness.quiescent() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return liveness.quiescent();
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> AcceptanceIntent {
        AcceptanceIntent {
            intent_version: ACCEPTANCE_INTENT_VERSION,
            run_id: "run-1".into(),
            request_id: "req-1".into(),
            payload_hash: hash_payload(&serde_json::json!({"a": 1})),
            tool: "ptah_submit_task".into(),
            session_id: Uuid::nil(),
            session_revision: "3".into(),
            workspace: "/tmp/project".into(),
            workspace_revision: "rev-1".into(),
            agent_id: None,
            agent_revision: 0,
            spec_revision: "bridge/1".into(),
            prompt: "fix the failing test".into(),
            bounds: SealedBounds {
                max_prompt_bytes: 1000,
                max_rounds: 2,
                max_duration_ms: 1000,
            },
            execution_mode: RunExecutionMode::Shared,
            allow_queue: true,
            retry_of: None,
            parent_run_id: None,
            created_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            digest: String::new(),
        }
        .seal()
    }

    #[test]
    fn sealed_intent_round_trips_and_validates() {
        let sealed = intent();
        assert!(sealed.validate().is_ok());
        let encoded = serde_json::to_vec(&sealed).unwrap();
        let decoded: AcceptanceIntent = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.validate().is_ok());
        assert_eq!(decoded, sealed);
    }

    /// One named, individually parseable edit to a sealed record.
    type IntentMutation = Box<dyn Fn(&mut AcceptanceIntent)>;

    #[test]
    fn every_execution_relevant_field_is_sealed() {
        let base = intent();
        // Each mutation is individually parseable JSON; the seal is what
        // rejects it. A field missing from this list is a field an attacker
        // could rewrite between acceptance and dispatch.
        let mutations: Vec<(&str, IntentMutation)> = vec![
            (
                "prompt",
                Box::new(|i: &mut AcceptanceIntent| i.prompt = "rm -rf /".into()),
            ),
            (
                "prompt_length_only",
                Box::new(|i: &mut AcceptanceIntent| i.prompt.push(' ')),
            ),
            (
                "run_id",
                Box::new(|i: &mut AcceptanceIntent| i.run_id = "run-2".into()),
            ),
            (
                "request_id",
                Box::new(|i: &mut AcceptanceIntent| i.request_id = "req-2".into()),
            ),
            (
                "payload_hash",
                Box::new(|i: &mut AcceptanceIntent| {
                    i.payload_hash = hash_payload(&serde_json::json!({"a": 2}))
                }),
            ),
            (
                "tool",
                Box::new(|i: &mut AcceptanceIntent| i.tool = "ptah_retry_run".into()),
            ),
            (
                "session_id",
                Box::new(|i: &mut AcceptanceIntent| i.session_id = Uuid::from_u128(7)),
            ),
            (
                "session_revision",
                Box::new(|i: &mut AcceptanceIntent| i.session_revision = "4".into()),
            ),
            (
                "workspace",
                Box::new(|i: &mut AcceptanceIntent| i.workspace = "/tmp/other".into()),
            ),
            (
                "workspace_revision",
                Box::new(|i: &mut AcceptanceIntent| i.workspace_revision = "rev-2".into()),
            ),
            (
                "agent_id",
                Box::new(|i: &mut AcceptanceIntent| i.agent_id = Some("agent-9".into())),
            ),
            (
                "agent_revision",
                Box::new(|i: &mut AcceptanceIntent| i.agent_revision = 9),
            ),
            (
                "spec_revision",
                Box::new(|i: &mut AcceptanceIntent| i.spec_revision = "bridge/2".into()),
            ),
            (
                "bounds.max_rounds",
                Box::new(|i: &mut AcceptanceIntent| i.bounds.max_rounds = 64),
            ),
            (
                "bounds.max_duration_ms",
                Box::new(|i: &mut AcceptanceIntent| i.bounds.max_duration_ms = 86_400_000),
            ),
            (
                "bounds.max_prompt_bytes",
                Box::new(|i: &mut AcceptanceIntent| i.bounds.max_prompt_bytes = 999_999),
            ),
            (
                "execution_mode",
                Box::new(|i: &mut AcceptanceIntent| {
                    i.execution_mode = RunExecutionMode::IsolatedWorktree
                }),
            ),
            (
                "allow_queue",
                Box::new(|i: &mut AcceptanceIntent| i.allow_queue = !i.allow_queue),
            ),
            (
                "retry_of",
                Box::new(|i: &mut AcceptanceIntent| i.retry_of = Some("run-0".into())),
            ),
            (
                "parent_run_id",
                Box::new(|i: &mut AcceptanceIntent| i.parent_run_id = Some("run-0".into())),
            ),
            (
                "created_at",
                Box::new(|i: &mut AcceptanceIntent| i.created_at += chrono::Duration::seconds(1)),
            ),
            (
                "intent_version",
                Box::new(|i: &mut AcceptanceIntent| i.intent_version = 99),
            ),
        ];
        for (field, mutate) in mutations {
            let mut tampered = base.clone();
            mutate(&mut tampered);
            // Re-serialize so the tamper is a real, parseable record on disk.
            let encoded = serde_json::to_vec(&tampered).unwrap();
            let decoded: AcceptanceIntent = serde_json::from_slice(&encoded).unwrap();
            assert!(
                decoded.validate().is_err(),
                "tampering with {field} must fail closed"
            );
        }
    }

    #[test]
    fn unknown_and_missing_fields_fail_to_parse() {
        let sealed = intent();
        let mut value = serde_json::to_value(&sealed).unwrap();
        value.as_object_mut().unwrap().insert(
            "shadowPrompt".into(),
            serde_json::json!("do something else"),
        );
        assert!(serde_json::from_value::<AcceptanceIntent>(value).is_err());

        let mut bounds_extra = serde_json::to_value(&sealed).unwrap();
        bounds_extra["bounds"]
            .as_object_mut()
            .unwrap()
            .insert("maxTokens".into(), serde_json::json!(1));
        assert!(serde_json::from_value::<AcceptanceIntent>(bounds_extra).is_err());

        let mut missing = serde_json::to_value(&sealed).unwrap();
        missing.as_object_mut().unwrap().remove("bounds");
        assert!(serde_json::from_value::<AcceptanceIntent>(missing).is_err());
    }

    #[test]
    fn prompt_must_stay_within_its_sealed_bound() {
        let mut over = intent();
        over.prompt = "x".repeat(over.bounds.max_prompt_bytes + 1);
        let over = over.seal();
        // The seal itself is valid; the bound is what rejects it.
        assert_eq!(over.digest, over.digest_for());
        assert!(over.validate().is_err());
    }

    fn lease(now: DateTime<Utc>) -> AttemptLease {
        AttemptLease {
            lease_version: ATTEMPT_LEASE_VERSION,
            run_id: "run-1".into(),
            attempt: 1,
            attempt_id: "attempt-1".into(),
            owner_id: "owner-1".into(),
            session_id: Uuid::nil(),
            intent_digest: intent().digest,
            acquired_at: now,
            heartbeat_at: now,
            lease_ttl_ms: 1_000,
            state: AttemptLeaseState::Held,
            digest: String::new(),
        }
        .seal()
    }

    #[test]
    fn lease_expiry_is_evaluated_against_the_durable_heartbeat() {
        let now = Utc::now();
        let held = lease(now);
        assert!(held.validate().is_ok());
        assert!(held.is_active(now));
        assert!(!held.is_expired(now));
        assert!(held.is_expired(now + chrono::Duration::milliseconds(1_001)));

        let released = AttemptLease {
            state: AttemptLeaseState::Released,
            ..held.clone()
        }
        .seal();
        assert!(released.is_expired(now));
        assert!(!released.is_active(now));
    }

    #[test]
    fn lease_seal_covers_owner_attempt_and_state() {
        let now = Utc::now();
        let base = lease(now);
        for mutate in [
            Box::new(|l: &mut AttemptLease| l.owner_id = "owner-2".into())
                as Box<dyn Fn(&mut AttemptLease)>,
            Box::new(|l: &mut AttemptLease| l.attempt_id = "attempt-2".into()),
            Box::new(|l: &mut AttemptLease| l.attempt = 2),
            Box::new(|l: &mut AttemptLease| l.state = AttemptLeaseState::Released),
            Box::new(|l: &mut AttemptLease| l.lease_ttl_ms = u64::MAX),
            Box::new(|l: &mut AttemptLease| l.intent_digest = hash_payload(&serde_json::json!(0))),
            Box::new(|l: &mut AttemptLease| l.session_id = Uuid::from_u128(3)),
            Box::new(|l: &mut AttemptLease| l.heartbeat_at += chrono::Duration::seconds(600)),
        ] {
            let mut tampered = base.clone();
            mutate(&mut tampered);
            assert!(tampered.validate().is_err());
        }
    }

    #[test]
    fn settlement_happens_exactly_once_under_contention() {
        let worker = Arc::new(LiveWorker::new(
            "run-1".into(),
            "attempt-1".into(),
            1,
            Uuid::nil(),
            CancellationToken::new(),
            Arc::new(WorkerLiveness::default()),
        ));
        let winners: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let worker = worker.clone();
                    scope.spawn(move || usize::from(worker.settle_once()))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(winners, 1, "exactly one settlement must win");
        assert!(worker.is_settled());
    }

    // Real time on purpose: the worker below is a blocking task, and a paused
    // clock never auto-advances while one is outstanding, so the bounded wait
    // under test would never elapse.
    #[tokio::test]
    async fn terminate_reports_worker_escape_when_cancellation_is_ignored() {
        let liveness = Arc::new(WorkerLiveness::default());
        let cancel = CancellationToken::new();
        let entry = LiveWorker::new(
            "run-1".into(),
            "attempt-1".into(),
            1,
            Uuid::nil(),
            cancel.clone(),
            liveness.clone(),
        );

        // A worker that ignores its cancel token and never yields to the
        // runtime: tokio cannot abort it, so teardown must say so rather than
        // pretend the future is gone.
        let hold = Arc::new(AtomicBool::new(true));
        let hold_worker = hold.clone();
        let liveness_worker = liveness.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let _guard = WorkerLivenessGuard::new(liveness_worker);
            while hold_worker.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        entry.attach_worker(worker.abort_handle());
        // Give the blocking worker a real chance to start before teardown.
        while !liveness.started() {
            tokio::task::yield_now().await;
        }

        let outcome = entry.terminate(Duration::from_millis(50)).await;
        assert_eq!(outcome, TerminationOutcome::WorkerEscaped);
        assert!(
            !outcome.may_release_capacity(),
            "capacity must not be reused while the old future can still run"
        );
        assert!(
            cancel.is_cancelled(),
            "cancellation must still be signalled"
        );

        hold.store(false, Ordering::Release);
        let _ = worker.await;
        assert!(liveness.finished());
    }

    #[tokio::test]
    async fn terminate_confirms_a_cooperative_worker_and_supervisor() {
        let liveness = Arc::new(WorkerLiveness::default());
        let cancel = CancellationToken::new();
        let entry = Arc::new(LiveWorker::new(
            "run-1".into(),
            "attempt-1".into(),
            1,
            Uuid::nil(),
            cancel.clone(),
            liveness.clone(),
        ));

        let liveness_worker = liveness.clone();
        let cancel_worker = cancel.clone();
        let worker = tokio::spawn(async move {
            let _guard = WorkerLivenessGuard::new(liveness_worker);
            cancel_worker.cancelled().await;
        });
        entry.attach_worker(worker.abort_handle());
        // Let the worker reach its first poll, so this exercises "a started
        // future ended" rather than "a future was cancelled before it ran".
        while !liveness.started() {
            tokio::task::yield_now().await;
        }
        let supervisor = tokio::spawn(async move {
            let _ = worker.await;
        });
        entry.attach_supervisor(supervisor);

        let outcome = entry.terminate(DEFAULT_TEARDOWN_BUDGET).await;
        assert_eq!(outcome, TerminationOutcome::Terminated);
        assert!(outcome.may_release_capacity());
        assert!(liveness.finished(), "worker future must actually be gone");
    }

    #[tokio::test]
    async fn a_worker_cancelled_before_its_first_poll_is_quiescent() {
        let liveness = Arc::new(WorkerLiveness::default());
        let entry = LiveWorker::new(
            "run-1".into(),
            "attempt-1".into(),
            1,
            Uuid::nil(),
            CancellationToken::new(),
            liveness.clone(),
        );
        let liveness_worker = liveness.clone();
        // Never awaited before teardown, so on a current-thread runtime the
        // body is never polled at all.
        let worker = tokio::spawn(async move {
            let _guard = WorkerLivenessGuard::new(liveness_worker);
            futures::future::pending::<()>().await;
        });
        entry.attach_worker(worker.abort_handle());

        let outcome = entry.terminate(DEFAULT_TEARDOWN_BUDGET).await;
        assert_eq!(outcome, TerminationOutcome::Terminated);
        assert!(
            liveness.quiescent(),
            "a future cancelled before its first poll can never execute"
        );
        assert!(!liveness.started(), "its body must never have run");
        assert!(worker.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn terminate_aborts_a_worker_that_is_parked_on_an_await() {
        let liveness = Arc::new(WorkerLiveness::default());
        let entry = LiveWorker::new(
            "run-1".into(),
            "attempt-1".into(),
            1,
            Uuid::nil(),
            CancellationToken::new(),
            liveness.clone(),
        );
        let liveness_worker = liveness.clone();
        let worker = tokio::spawn(async move {
            let _guard = WorkerLivenessGuard::new(liveness_worker);
            // Ignores the cancel token entirely, but does yield.
            futures::future::pending::<()>().await;
        });
        entry.attach_worker(worker.abort_handle());
        while !liveness.started() {
            tokio::task::yield_now().await;
        }

        let outcome = entry.terminate(DEFAULT_TEARDOWN_BUDGET).await;
        assert_eq!(outcome, TerminationOutcome::Terminated);
        assert!(liveness.finished());
        assert!(worker.await.unwrap_err().is_cancelled());
    }
}
