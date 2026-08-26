//! Durable state for one work graph.
//!
//! Every field is written before the side effect it describes. A lease exists
//! before a worker is told it owns anything, and an attempt row exists before
//! the host enters a provider transport, so a crash between the write and the
//! effect leaves evidence that something *may* have happened — which the
//! scheduler treats as uncertain rather than free to repeat.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::orchestration::types::{hash_payload, OrchError, OrchErrorCode};

use super::authority::{ActionAuthority, ProviderAttemptRecord};
use super::ids::{AttemptId, AuthorityId, GrantId, GraphId, LeaseId, WorkId, WorkerId};
use super::spec::{WorkGraphSpec, WorkerRole};

pub const GRAPH_STATE_SCHEMA_VERSION: u32 = 1;

/// Maximum evidence entries retained per work item.
pub const MAX_EVIDENCE_ENTRIES: usize = 16;
pub const MAX_EVIDENCE_LABEL_BYTES: usize = 128;
pub const MAX_EVIDENCE_DETAIL_BYTES: usize = 2 * 1024;
pub const MAX_SUMMARY_BYTES: usize = 4 * 1024;
pub const MAX_REASON_BYTES: usize = 1024;

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

/// Truncate on a character boundary. Never splits a codepoint.
pub fn truncate_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// Where the whole graph stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphLifecycle {
    Active,
    /// A whole-graph cancel was requested; admission has stopped and live
    /// children are winding down.
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    /// Every remaining item is blocked or discarded after review.
    Discarded,
}

impl GraphLifecycle {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Discarded
        )
    }
}

/// Where one node of the graph stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// Dependencies are not satisfied yet.
    Pending,
    /// Dependencies are satisfied; eligible for admission.
    Ready,
    /// A lease was written but the worker has not acknowledged it.
    Leased,
    /// The worker acknowledged and is running.
    Running,
    /// A cancel was requested for a live child; awaiting confirmation.
    Cancelling,
    Succeeded,
    Failed,
    /// An upstream failure, cancellation, or unresolved uncertainty means this
    /// item can never become ready. Derived, not sticky.
    Blocked,
    Cancelled,
    /// The item exceeded its execution bound.
    TimedOut,
    /// A dispatch was attempted and its fate is unknown. The child may be
    /// running. Never re-dispatched without external evidence.
    DispatchUncertain,
    /// Reviewed and explicitly discarded. Terminal and truthful: a discarded
    /// item never counts as a success.
    Discarded,
}

impl WorkState {
    /// True once the outcome will not change on its own.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut | Self::Discarded
        )
    }

    /// States the scheduler recomputes from upstream results.
    pub fn is_derived(self) -> bool {
        matches!(self, Self::Pending | Self::Ready | Self::Blocked)
    }

    /// True while the item may still hold live capacity.
    ///
    /// `DispatchUncertain` counts: a child whose fate is unknown may still be
    /// running, so the admission slot it holds is never reissued.
    pub fn occupies_slot(self) -> bool {
        matches!(
            self,
            Self::Leased | Self::Running | Self::Cancelling | Self::DispatchUncertain
        )
    }
}

/// A reviewer's verdict. Recorded independently of whether the reviewer
/// succeeded: a reviewer that ran to completion and rejected has done its job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    Reject,
}

/// How a work item finished, as reported by its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkResult {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

/// One bounded piece of evidence.
///
/// Both fields are free-form text authored outside the host, so both are
/// redacted and bounded before they can reach any projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceEntry {
    pub label: String,
    pub detail: String,
}

impl EvidenceEntry {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
        }
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.label.is_empty() || self.label.len() > MAX_EVIDENCE_LABEL_BYTES {
            return Err(invalid("evidence label is empty or exceeds its bound"));
        }
        if self.detail.len() > MAX_EVIDENCE_DETAIL_BYTES {
            return Err(invalid("evidence detail exceeds its bound"));
        }
        if self.label.contains('\0') || self.detail.contains('\0') {
            return Err(invalid("evidence must not contain NUL"));
        }
        Ok(())
    }
}

/// A terminal report from a worker, tied to the lease that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkOutcome {
    pub result: WorkResult,
    /// Required for a review item, rejected for every other role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceEntry>,
}

impl WorkOutcome {
    pub fn succeeded() -> Self {
        Self {
            result: WorkResult::Succeeded,
            verdict: None,
            summary: None,
            evidence: Vec::new(),
        }
    }

    pub fn failed(summary: impl Into<String>) -> Self {
        Self {
            result: WorkResult::Failed,
            verdict: None,
            summary: Some(summary.into()),
            evidence: Vec::new(),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            result: WorkResult::Cancelled,
            verdict: None,
            summary: None,
            evidence: Vec::new(),
        }
    }

    pub fn with_verdict(mut self, verdict: ReviewVerdict) -> Self {
        self.verdict = Some(verdict);
        self
    }

    pub fn with_evidence(mut self, evidence: Vec<EvidenceEntry>) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if let Some(summary) = &self.summary {
            if summary.len() > MAX_SUMMARY_BYTES || summary.contains('\0') {
                return Err(invalid("outcome summary is invalid"));
            }
        }
        if self.evidence.len() > MAX_EVIDENCE_ENTRIES {
            return Err(invalid(format!(
                "an outcome carries at most {MAX_EVIDENCE_ENTRIES} evidence entries"
            )));
        }
        for entry in &self.evidence {
            entry.validate()?;
        }
        Ok(())
    }
}

/// Where one lease stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    /// Written before the spawn. Whether the worker started is unknown.
    Issued,
    /// One caller won the durable right to perform the external spawn.
    Claimed,
    /// The worker acknowledged and reported a handle.
    Acknowledged,
    /// The worker reported a terminal outcome.
    Settled,
    /// The lease's fate is unknown and it will not be retried.
    Uncertain,
    /// Superseded by a later epoch or explicitly revoked.
    Revoked,
}

/// A durable, single-owner claim on one work item for one attempt.
///
/// This is the duplicate-suppression record: a second worker presenting the
/// same lease loses, and a worker presenting a lease from an earlier epoch is
/// stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRecord {
    pub lease_id: LeaseId,
    pub graph_id: GraphId,
    pub work_id: WorkId,
    pub worker_id: WorkerId,
    pub attempt_id: AttemptId,
    pub attempt: u32,
    pub authority_id: AuthorityId,
    pub session_id: Uuid,
    pub workspace: String,
    /// Monotonic fence. A cancel, takeover, timeout, or restart bumps it, so a
    /// lease minted under an older epoch is refused even if it is otherwise
    /// well formed.
    pub epoch: u64,
    pub state: LeaseState,
    /// Opaque, non-secret handle the owner reported on acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// The Computer Use grant that authorized this lease, when the item
    /// required one. Recorded for audit; never issued or extended here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<GrantBinding>,
    pub issued_at: DateTime<Utc>,
    /// Hard execution deadline for this lease.
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertain_reason: Option<String>,
}

impl LeaseRecord {
    pub fn validate(&self) -> Result<(), OrchError> {
        self.lease_id.validate()?;
        self.graph_id.validate()?;
        self.work_id.validate()?;
        self.worker_id.validate()?;
        self.attempt_id.validate()?;
        self.authority_id.validate()?;
        if self.workspace.is_empty() || self.workspace.len() > 4096 {
            return Err(invalid("lease workspace is invalid"));
        }
        if self.expires_at <= self.issued_at {
            return Err(invalid("lease has a non-positive lifetime"));
        }
        if let Some(reason) = &self.uncertain_reason {
            if reason.len() > MAX_REASON_BYTES {
                return Err(invalid("lease uncertainty reason exceeds its bound"));
            }
        }
        if let Some(grant) = &self.grant {
            grant.validate()?;
        }
        Ok(())
    }

    /// True while this lease may still describe a live child.
    pub fn is_live(&self) -> bool {
        matches!(
            self.state,
            LeaseState::Issued | LeaseState::Claimed | LeaseState::Acknowledged
        )
    }
}

/// Reference to an externally issued Computer Use grant, bound to exactly one
/// lease.
///
/// The graph never issues, extends, or revalidates the grant: that authority
/// belongs to the Computer Use ledger. What is recorded here is the exact
/// binding the single consumption path must verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantBinding {
    pub grant_id: GrantId,
    /// The Computer Use run the grant was issued against.
    pub computer_run_id: String,
    /// Digest of the exact target the grant names.
    pub target_fingerprint: String,
    pub owner_session_id: Uuid,
    /// The Computer Use run's control epoch at binding time. A pause, takeover,
    /// stop, or recovery bumps it, which makes this binding stale.
    pub control_epoch: u64,
    pub binding_hash: String,
}

impl GrantBinding {
    pub fn new(
        grant_id: GrantId,
        computer_run_id: impl Into<String>,
        target_fingerprint: impl Into<String>,
        owner_session_id: Uuid,
        control_epoch: u64,
        lease_id: &LeaseId,
        attempt_id: &AttemptId,
    ) -> Result<Self, OrchError> {
        let mut binding = Self {
            grant_id,
            computer_run_id: computer_run_id.into(),
            target_fingerprint: target_fingerprint.into(),
            owner_session_id,
            control_epoch,
            binding_hash: String::new(),
        };
        binding.binding_hash = binding.expected_hash(lease_id, attempt_id);
        binding.validate()?;
        Ok(binding)
    }

    fn expected_hash(&self, lease_id: &LeaseId, attempt_id: &AttemptId) -> String {
        hash_payload(&serde_json::json!({
            "grantId": self.grant_id,
            "computerRunId": self.computer_run_id,
            "targetFingerprint": self.target_fingerprint,
            "ownerSessionId": self.owner_session_id,
            "controlEpoch": self.control_epoch,
            "leaseId": lease_id,
            "attemptId": attempt_id,
        }))
    }

    /// Verify the binding still names this exact lease and attempt.
    pub fn verify_binding(
        &self,
        lease_id: &LeaseId,
        attempt_id: &AttemptId,
    ) -> Result<(), OrchError> {
        if self.binding_hash != self.expected_hash(lease_id, attempt_id) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "computer use grant binding does not name this lease",
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        self.grant_id.validate()?;
        if self.computer_run_id.is_empty() || self.computer_run_id.len() > 256 {
            return Err(invalid("grant binding computer run id is invalid"));
        }
        if self.target_fingerprint.len() != 64
            || !self
                .target_fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err(invalid("grant binding target fingerprint is invalid"));
        }
        if self.binding_hash.len() != 64
            || !self.binding_hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(invalid("grant binding hash is invalid"));
        }
        Ok(())
    }
}

/// Durable per-work-item state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRecord {
    pub work_id: WorkId,
    pub state: WorkState,
    /// Dispatch attempts made so far; feeds the deterministic attempt identity.
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_lease_id: Option<LeaseId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceEntry>,
    /// Per-work send ordinal high-water mark.
    #[serde(default)]
    pub send_ordinal: u64,
    pub updated_at: DateTime<Utc>,
}

impl WorkRecord {
    pub fn new(work_id: WorkId, now: DateTime<Utc>) -> Self {
        Self {
            work_id,
            state: WorkState::Pending,
            attempts: 0,
            current_lease_id: None,
            verdict: None,
            summary: None,
            last_error: None,
            evidence: Vec::new(),
            send_ordinal: 0,
            updated_at: now,
        }
    }

    /// Claim the next send ordinal with `checked_add`.
    ///
    /// A work item that exhausts the ordinal space stops rather than reusing a
    /// position that a durable attempt row already published.
    pub fn claim_send_ordinal(&mut self) -> Result<u64, OrchError> {
        let next = self.send_ordinal.checked_add(1).ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::CapacityExhausted,
                "work item exhausted its provider send ordinal space",
            )
        })?;
        self.send_ordinal = next;
        Ok(next)
    }
}

/// Bounded ledger of what the graph has already consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BudgetLedger {
    pub attempts_used: u32,
    pub tokens_used: u64,
    /// Attempts refused because a budget was exhausted. Surfaced so an operator
    /// can tell "nothing to do" apart from "not allowed to do it".
    #[serde(default)]
    pub admissions_refused: u64,
}

/// The complete durable record for one work graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkGraphRecord {
    pub schema_version: u32,
    pub graph_id: GraphId,
    pub session_id: Uuid,
    /// Canonical workspace this graph is bound to.
    pub workspace: String,
    pub agent_id: String,
    /// Monotonic revision for compare-and-swap persistence.
    pub revision: u64,
    pub lifecycle: GraphLifecycle,
    pub spec: WorkGraphSpec,
    pub work: Vec<WorkRecord>,
    #[serde(default)]
    pub leases: Vec<LeaseRecord>,
    #[serde(default)]
    pub attempts: Vec<ProviderAttemptRecord>,
    #[serde(default)]
    pub authorities: Vec<ActionAuthority>,
    #[serde(default)]
    pub budget: BudgetLedger,
    /// Monotonic control fence for the whole graph.
    #[serde(default)]
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Hard deadline derived from the graph budget.
    pub deadline_at: DateTime<Utc>,
}

impl WorkGraphRecord {
    pub fn new(
        graph_id: GraphId,
        session_id: Uuid,
        workspace: impl Into<String>,
        agent_id: impl Into<String>,
        spec: WorkGraphSpec,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        spec.validate()?;
        let work = spec
            .work
            .iter()
            .map(|item| WorkRecord::new(item.work_id.clone(), now))
            .collect();
        let deadline_at = now
            .checked_add_signed(chrono::Duration::milliseconds(
                spec.budget.max_wall_clock_ms as i64,
            ))
            .ok_or_else(|| invalid("graph deadline overflows"))?;
        let record = Self {
            schema_version: GRAPH_STATE_SCHEMA_VERSION,
            graph_id,
            session_id,
            workspace: workspace.into(),
            agent_id: agent_id.into(),
            revision: 1,
            lifecycle: GraphLifecycle::Active,
            spec,
            work,
            leases: Vec::new(),
            attempts: Vec::new(),
            authorities: Vec::new(),
            budget: BudgetLedger::default(),
            epoch: 0,
            stop_reason: None,
            created_at: now,
            updated_at: now,
            deadline_at,
        };
        record.validate()?;
        Ok(record)
    }

    /// Validate a record that arrived from disk before it is trusted.
    ///
    /// A malformed durable record fails closed here rather than being partially
    /// honored: every identity is re-validated, and cross-references between
    /// work, leases, attempts, and authorities must resolve.
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != GRAPH_STATE_SCHEMA_VERSION {
            return Err(invalid("work graph state schema version is not supported"));
        }
        self.graph_id.validate()?;
        if self.workspace.is_empty() || self.workspace.len() > 4096 {
            return Err(invalid("graph workspace is invalid"));
        }
        if self.agent_id.is_empty() || self.agent_id.len() > 256 {
            return Err(invalid("graph agent id is invalid"));
        }
        if self.revision == 0 {
            return Err(invalid("graph revision must be >= 1"));
        }
        self.spec.validate()?;
        if self.work.len() != self.spec.work.len() {
            return Err(invalid("graph work records do not match the specification"));
        }
        let declared: BTreeSet<&WorkId> = self.spec.work.iter().map(|item| &item.work_id).collect();
        let mut seen = BTreeSet::new();
        for record in &self.work {
            record.work_id.validate()?;
            if !declared.contains(&record.work_id) {
                return Err(invalid(format!(
                    "graph carries a record for undeclared work {}",
                    record.work_id
                )));
            }
            if !seen.insert(&record.work_id) {
                return Err(invalid(format!(
                    "graph carries duplicate records for work {}",
                    record.work_id
                )));
            }
            if record.evidence.len() > MAX_EVIDENCE_ENTRIES {
                return Err(invalid("work record evidence exceeds its bound"));
            }
            for entry in &record.evidence {
                entry.validate()?;
            }
            if let Some(lease_id) = &record.current_lease_id {
                lease_id.validate()?;
                if !self.leases.iter().any(|lease| &lease.lease_id == lease_id) {
                    return Err(invalid(format!(
                        "work {} names a lease that is not in the ledger",
                        record.work_id
                    )));
                }
            }
            if record.verdict.is_some()
                && self.spec.role_of(&record.work_id) != Some(WorkerRole::Review)
            {
                return Err(invalid(format!(
                    "work {} carries a verdict but is not a review item",
                    record.work_id
                )));
            }
        }
        let mut lease_ids = BTreeSet::new();
        for lease in &self.leases {
            lease.validate()?;
            if lease.graph_id != self.graph_id {
                return Err(invalid("lease belongs to a different graph"));
            }
            if !declared.contains(&lease.work_id) {
                return Err(invalid("lease names undeclared work"));
            }
            if !lease_ids.insert(&lease.lease_id) {
                return Err(invalid("duplicate lease id in the ledger"));
            }
        }
        let mut attempt_ids = BTreeSet::new();
        for attempt in &self.attempts {
            attempt.validate()?;
            if attempt.graph_id != self.graph_id {
                return Err(invalid("attempt belongs to a different graph"));
            }
            if !declared.contains(&attempt.work_id) {
                return Err(invalid("attempt names undeclared work"));
            }
            if !attempt_ids.insert(&attempt.attempt_id) {
                return Err(invalid("duplicate attempt id in the ledger"));
            }
        }
        let mut authority_ids = BTreeSet::new();
        for authority in &self.authorities {
            authority.validate()?;
            if authority.graph_id != self.graph_id
                || authority.session_id != self.session_id
                || authority.workspace != self.workspace
                || authority.agent_id != self.agent_id
            {
                return Err(invalid("authority is not bound to this graph"));
            }
            if !authority_ids.insert(&authority.authority_id) {
                return Err(invalid("duplicate authority id in the ledger"));
            }
        }
        if self.deadline_at <= self.created_at {
            return Err(invalid("graph deadline is not after its creation"));
        }
        Ok(())
    }

    pub fn work_record(&self, work_id: &WorkId) -> Option<&WorkRecord> {
        self.work.iter().find(|record| &record.work_id == work_id)
    }

    pub fn work_record_mut(&mut self, work_id: &WorkId) -> Option<&mut WorkRecord> {
        self.work
            .iter_mut()
            .find(|record| &record.work_id == work_id)
    }

    pub fn lease(&self, lease_id: &LeaseId) -> Option<&LeaseRecord> {
        self.leases.iter().find(|lease| &lease.lease_id == lease_id)
    }

    pub fn lease_mut(&mut self, lease_id: &LeaseId) -> Option<&mut LeaseRecord> {
        self.leases
            .iter_mut()
            .find(|lease| &lease.lease_id == lease_id)
    }

    pub fn attempt(&self, attempt_id: &AttemptId) -> Option<&ProviderAttemptRecord> {
        self.attempts
            .iter()
            .find(|attempt| &attempt.attempt_id == attempt_id)
    }

    pub fn attempt_mut(&mut self, attempt_id: &AttemptId) -> Option<&mut ProviderAttemptRecord> {
        self.attempts
            .iter_mut()
            .find(|attempt| &attempt.attempt_id == attempt_id)
    }

    pub fn authority(&self, authority_id: &AuthorityId) -> Option<&ActionAuthority> {
        self.authorities
            .iter()
            .find(|authority| &authority.authority_id == authority_id)
    }

    /// Count of items that may still hold live capacity.
    pub fn in_flight(&self) -> usize {
        self.work
            .iter()
            .filter(|record| record.state.occupies_slot())
            .count()
    }

    /// True while any item's fate is unknown. The graph refuses to declare a
    /// terminal outcome — including a completed cancellation — while this holds.
    pub fn has_uncertainty(&self) -> bool {
        self.work
            .iter()
            .any(|record| record.state == WorkState::DispatchUncertain)
    }
}
