//! Transport-neutral durable workload records and state transitions (#305).
//!
//! A WorkItem is durable intent. A Lane/session and finite Run are execution
//! projections linked by ID; neither focused UI state nor Lane visibility is
//! the source of truth for workload ownership.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{hash_payload, OrchError, OrchErrorCode, RunBounds};

pub const WORKLOAD_SCHEMA_VERSION: u32 = 1;
pub const MAX_WORK_KIND_BYTES: usize = 96;
pub const MAX_WORK_OBJECTIVE_BYTES: usize = 32 * 1024;
pub const MAX_WORK_ACTOR_BYTES: usize = 256;
pub const MAX_WORK_ARTIFACTS: usize = 256;
pub const MAX_WORK_ARTIFACT_BYTES: usize = 2 * 1024;
pub const MAX_WORK_DEPENDENCIES: usize = 128;
pub const DEFAULT_WORK_LEASE_MS: u64 = 5 * 60 * 1_000;
pub const MAX_WORK_LEASE_MS: u64 = 60 * 60 * 1_000;
pub const DEFAULT_WORK_MAX_ATTEMPTS: u32 = 3;

/// Counts the durable changes made by one workload reconciliation pass.
///
/// The report is intentionally transport-neutral: a local desktop, the
/// headless service, and a future hosted scheduler can expose the same
/// evidence without sharing a process-local queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadReconciliationReport {
    pub scanned_items: usize,
    pub expired_attempts: usize,
    pub retried_items: usize,
    pub failed_items: usize,
    pub blocked_items: usize,
    pub unblocked_items: usize,
    pub deadline_failed_items: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    #[default]
    Queued,
    Blocked,
    Leased,
    Running,
    AwaitingInput,
    AwaitingApproval,
    Review,
    Succeeded,
    Failed,
    Cancelled,
}

impl WorkState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub fn is_claimable(self) -> bool {
        matches!(self, Self::Queued)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Leased,
    Running,
    AwaitingInput,
    AwaitingApproval,
    Review,
    Released,
    Expired,
    Succeeded,
    Failed,
    Cancelled,
}

impl AttemptState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Leased
                | Self::Running
                | Self::AwaitingInput
                | Self::AwaitingApproval
                | Self::Review
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRetryPolicy {
    pub max_attempts: u32,
    pub retry_failed: bool,
    pub retry_expired: bool,
    pub backoff_ms: u64,
}

impl Default for WorkRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_WORK_MAX_ATTEMPTS,
            retry_failed: true,
            retry_expired: true,
            backoff_ms: 0,
        }
    }
}

impl WorkRetryPolicy {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.max_attempts == 0 {
            return Err(invalid("retry.max_attempts must be greater than zero"));
        }
        if self.max_attempts > 100 {
            return Err(invalid("retry.max_attempts exceeds the workload bound"));
        }
        if self.backoff_ms > 24 * 60 * 60 * 1_000 {
            return Err(invalid("retry.backoff_ms exceeds one day"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkPolicy {
    pub bounds: RunBounds,
    pub retry: WorkRetryPolicy,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub max_concurrent_attempts: u32,
}

impl Default for WorkPolicy {
    fn default() -> Self {
        Self {
            bounds: RunBounds::default(),
            retry: WorkRetryPolicy::default(),
            requires_approval: false,
            max_concurrent_attempts: 1,
        }
    }
}

impl WorkPolicy {
    pub fn validate(&self) -> Result<(), OrchError> {
        self.bounds.validate()?;
        self.retry.validate()?;
        if self.max_concurrent_attempts == 0 || self.max_concurrent_attempts > 1 {
            return Err(invalid(
                "policy.max_concurrent_attempts must be exactly one in the single-owner runtime",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkDependency {
    pub work_id: String,
    #[serde(default)]
    pub required_state: WorkState,
}

impl WorkDependency {
    pub fn validate(&self) -> Result<(), OrchError> {
        validate_id(&self.work_id, "dependency.work_id")?;
        if self.required_state != WorkState::Succeeded {
            return Err(invalid(
                "dependency.required_state must be succeeded in this slice",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkProgress {
    pub summary: String,
    pub percent: Option<u8>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkArtifactRef {
    pub artifact_id: String,
    pub kind: String,
    pub label: String,
    pub digest: Option<String>,
    pub retained_until: Option<DateTime<Utc>>,
}

impl WorkArtifactRef {
    pub fn validate(&self) -> Result<(), OrchError> {
        validate_id(&self.artifact_id, "artifact.artifact_id")?;
        validate_text(&self.kind, MAX_WORK_ARTIFACT_BYTES, "artifact.kind")?;
        validate_text(&self.label, MAX_WORK_ARTIFACT_BYTES, "artifact.label")?;
        if let Some(digest) = &self.digest {
            validate_text(digest, MAX_WORK_ARTIFACT_BYTES, "artifact.digest")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkResult {
    pub summary: String,
    pub evidence: Vec<String>,
    pub artifacts: Vec<WorkArtifactRef>,
    pub failure: Option<String>,
    pub cancellation_reason: Option<String>,
    pub completed_at: DateTime<Utc>,
}

impl WorkResult {
    pub fn validate(&self) -> Result<(), OrchError> {
        validate_text(&self.summary, MAX_WORK_OBJECTIVE_BYTES, "result.summary")?;
        if self.evidence.len() > MAX_WORK_ARTIFACTS {
            return Err(invalid("result.evidence exceeds the workload bound"));
        }
        for evidence in &self.evidence {
            validate_text(evidence, MAX_WORK_ARTIFACT_BYTES, "result.evidence")?;
        }
        if self.artifacts.len() > MAX_WORK_ARTIFACTS {
            return Err(invalid("result.artifacts exceeds the workload bound"));
        }
        for artifact in &self.artifacts {
            artifact.validate()?;
        }
        if let Some(failure) = &self.failure {
            validate_text(failure, MAX_WORK_OBJECTIVE_BYTES, "result.failure")?;
        }
        if let Some(reason) = &self.cancellation_reason {
            validate_text(
                reason,
                MAX_WORK_OBJECTIVE_BYTES,
                "result.cancellation_reason",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub schema_version: u32,
    pub work_id: String,
    pub kind: String,
    pub objective: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub created_by: String,
    pub assigned_agent_id: Option<String>,
    pub priority: i32,
    pub deadline: Option<DateTime<Utc>>,
    pub parent_work_id: Option<String>,
    pub dependencies: Vec<WorkDependency>,
    pub policy: WorkPolicy,
    pub state: WorkState,
    pub revision: u64,
    pub attempt_count: u32,
    pub progress: Option<WorkProgress>,
    pub result: Option<WorkResult>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkItem {
    pub fn new(
        kind: impl Into<String>,
        objective: impl Into<String>,
        session_id: Uuid,
        workspace: impl Into<String>,
        created_by: impl Into<String>,
        policy: WorkPolicy,
    ) -> Result<Self, OrchError> {
        let now = Utc::now();
        let item = Self {
            schema_version: WORKLOAD_SCHEMA_VERSION,
            work_id: Uuid::new_v4().to_string(),
            kind: kind.into(),
            objective: objective.into(),
            session_id,
            workspace: workspace.into(),
            created_by: created_by.into(),
            assigned_agent_id: None,
            priority: 0,
            deadline: None,
            parent_work_id: None,
            dependencies: Vec::new(),
            policy,
            state: WorkState::Queued,
            revision: 1,
            attempt_count: 0,
            progress: None,
            result: None,
            created_at: now,
            updated_at: now,
        };
        item.validate()?;
        Ok(item)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != WORKLOAD_SCHEMA_VERSION || self.revision == 0 {
            return Err(invalid("work item schema version or revision is invalid"));
        }
        validate_id(&self.work_id, "work_id")?;
        validate_text(&self.kind, MAX_WORK_KIND_BYTES, "kind")?;
        validate_text(&self.objective, MAX_WORK_OBJECTIVE_BYTES, "objective")?;
        validate_text(&self.workspace, MAX_WORK_ACTOR_BYTES * 4, "workspace")?;
        validate_text(&self.created_by, MAX_WORK_ACTOR_BYTES, "created_by")?;
        if let Some(agent_id) = &self.assigned_agent_id {
            validate_id(agent_id, "assigned_agent_id")?;
        }
        if let Some(parent) = &self.parent_work_id {
            validate_id(parent, "parent_work_id")?;
            if parent == &self.work_id {
                return Err(invalid("work item cannot be its own parent"));
            }
        }
        if self.dependencies.len() > MAX_WORK_DEPENDENCIES {
            return Err(invalid("dependencies exceeds the workload bound"));
        }
        let mut dependency_ids = HashSet::new();
        for dependency in &self.dependencies {
            dependency.validate()?;
            if dependency.work_id == self.work_id {
                return Err(invalid("work item cannot depend on itself"));
            }
            if !dependency_ids.insert(&dependency.work_id) {
                return Err(invalid("duplicate workload dependency"));
            }
        }
        self.policy.validate()?;
        if self.attempt_count > self.policy.retry.max_attempts {
            return Err(invalid("attempt_count exceeds retry.max_attempts"));
        }
        if let Some(progress) = &self.progress {
            validate_text(
                &progress.summary,
                MAX_WORK_OBJECTIVE_BYTES,
                "progress.summary",
            )?;
            if progress.percent.is_some_and(|percent| percent > 100) {
                return Err(invalid("progress.percent must be between 0 and 100"));
            }
        }
        if let Some(result) = &self.result {
            result.validate()?;
        }
        Ok(())
    }

    pub fn dependency_ready(&self, dependencies: impl IntoIterator<Item = WorkState>) -> bool {
        self.dependencies.len()
            == dependencies
                .into_iter()
                .filter(|state| *state == WorkState::Succeeded)
                .count()
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.updated_at = Utc::now();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkAttempt {
    pub schema_version: u32,
    pub attempt_id: String,
    pub work_id: String,
    pub attempt_number: u32,
    pub claimant_id: String,
    pub lease_token_hash: String,
    pub acquired_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    pub last_heartbeat_at: DateTime<Utc>,
    pub state: AttemptState,
    pub linked_run_ids: Vec<String>,
    pub progress: Option<WorkProgress>,
    pub result: Option<WorkResult>,
    pub terminal_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkAttempt {
    pub fn new(work_id: &str, attempt_number: u32, claimant_id: &str, lease_token: &str) -> Self {
        let now = Utc::now();
        Self {
            schema_version: WORKLOAD_SCHEMA_VERSION,
            attempt_id: Uuid::new_v4().to_string(),
            work_id: work_id.to_string(),
            attempt_number,
            claimant_id: claimant_id.to_string(),
            lease_token_hash: hash_payload(&serde_json::json!({ "leaseToken": lease_token })),
            acquired_at: now,
            lease_expires_at: now + Duration::milliseconds(DEFAULT_WORK_LEASE_MS as i64),
            last_heartbeat_at: now,
            state: AttemptState::Leased,
            linked_run_ids: Vec::new(),
            progress: None,
            result: None,
            terminal_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn new_with_lease_secret(
        work_id: &str,
        attempt_number: u32,
        claimant_id: &str,
        lease_secret: &str,
    ) -> Self {
        let mut attempt = Self::new(work_id, attempt_number, claimant_id, "generated-at-claim");
        let lease_token = attempt.lease_token_for_secret(lease_secret);
        attempt.lease_token_hash = hash_payload(&serde_json::json!({ "leaseToken": lease_token }));
        attempt
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != WORKLOAD_SCHEMA_VERSION {
            return Err(invalid("work attempt schema version is invalid"));
        }
        validate_id(&self.attempt_id, "attempt_id")?;
        validate_id(&self.work_id, "attempt.work_id")?;
        if self.attempt_number == 0 {
            return Err(invalid("attempt_number must be greater than zero"));
        }
        validate_text(&self.claimant_id, MAX_WORK_ACTOR_BYTES, "claimant_id")?;
        validate_text(&self.lease_token_hash, 128, "lease_token_hash")?;
        if self.lease_expires_at < self.acquired_at {
            return Err(invalid("lease expires before it was acquired"));
        }
        if self.linked_run_ids.len() > 16 {
            return Err(invalid("linked_run_ids exceeds the workload bound"));
        }
        for run_id in &self.linked_run_ids {
            validate_id(run_id, "linked_run_id")?;
        }
        if let Some(progress) = &self.progress {
            validate_text(
                &progress.summary,
                MAX_WORK_OBJECTIVE_BYTES,
                "attempt.progress",
            )?;
        }
        if let Some(result) = &self.result {
            result.validate()?;
        }
        if let Some(reason) = &self.terminal_reason {
            validate_text(reason, MAX_WORK_OBJECTIVE_BYTES, "attempt.terminal_reason")?;
        }
        Ok(())
    }

    pub fn token_matches(&self, lease_token: &str) -> bool {
        self.lease_token_hash == hash_payload(&serde_json::json!({ "leaseToken": lease_token }))
    }

    pub fn lease_token_for_secret(&self, lease_secret: &str) -> String {
        hash_payload(&serde_json::json!({
            "leaseSecret": lease_secret,
            "workId": self.work_id,
            "attemptId": self.attempt_id,
            "attemptNumber": self.attempt_number,
        }))
    }

    pub fn lease_active_at(&self, now: DateTime<Utc>) -> bool {
        self.state.is_active() && now < self.lease_expires_at
    }
}

/// Network-safe projection of an attempt. The persisted lease-token hash is
/// deliberately omitted from every service/desktop response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkAttemptView {
    pub schema_version: u32,
    pub attempt_id: String,
    pub work_id: String,
    pub attempt_number: u32,
    pub claimant_id: String,
    pub acquired_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    pub last_heartbeat_at: DateTime<Utc>,
    pub state: AttemptState,
    pub linked_run_ids: Vec<String>,
    pub progress: Option<WorkProgress>,
    pub result: Option<WorkResult>,
    pub terminal_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&WorkAttempt> for WorkAttemptView {
    fn from(attempt: &WorkAttempt) -> Self {
        Self {
            schema_version: attempt.schema_version,
            attempt_id: attempt.attempt_id.clone(),
            work_id: attempt.work_id.clone(),
            attempt_number: attempt.attempt_number,
            claimant_id: attempt.claimant_id.clone(),
            acquired_at: attempt.acquired_at,
            lease_expires_at: attempt.lease_expires_at,
            last_heartbeat_at: attempt.last_heartbeat_at,
            state: attempt.state,
            linked_run_ids: attempt.linked_run_ids.clone(),
            progress: attempt.progress.clone(),
            result: attempt.result.clone(),
            terminal_reason: attempt.terminal_reason.clone(),
            created_at: attempt.created_at,
            updated_at: attempt.updated_at,
        }
    }
}

/// Complete desktop-safe projection for one durable Work Item.
///
/// The snapshot deliberately contains attempt history without the lease
/// credential. Local and remote clients can therefore render the same
/// operations view without gaining worker authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemSnapshot {
    pub work: WorkItem,
    pub attempts: Vec<WorkAttemptView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkClaim {
    pub work: WorkItem,
    pub attempt: WorkAttempt,
    /// Returned once to the worker; only its hash is persisted.
    pub lease_token: String,
}

pub fn lease_duration(milliseconds: Option<u64>) -> Result<Duration, OrchError> {
    let milliseconds = milliseconds.unwrap_or(DEFAULT_WORK_LEASE_MS);
    if milliseconds == 0 || milliseconds > MAX_WORK_LEASE_MS {
        return Err(invalid("lease_ms must be between 1 and 3600000"));
    }
    Ok(Duration::milliseconds(milliseconds as i64))
}

pub fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

fn validate_id(value: &str, field: &str) -> Result<(), OrchError> {
    if value.is_empty()
        || value.len() > 256
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(invalid(format!("{field} is empty or invalid")));
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
        return Err(invalid(format!("{field} is empty or exceeds its bound")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> WorkItem {
        WorkItem::new(
            "coding",
            "Run the conformance suite",
            Uuid::new_v4(),
            "/tmp/project",
            "operator",
            WorkPolicy::default(),
        )
        .unwrap()
    }

    #[test]
    fn workload_starts_queued_and_validates_bounds() {
        let item = work();
        assert_eq!(item.state, WorkState::Queued);
        assert!(item.validate().is_ok());
        assert!(lease_duration(Some(MAX_WORK_LEASE_MS + 1)).is_err());
    }

    #[test]
    fn lease_token_is_not_persisted_in_plaintext() {
        let attempt = WorkAttempt::new("work-1", 1, "worker-a", "secret-token");
        assert!(!serde_json::to_string(&attempt)
            .unwrap()
            .contains("secret-token"));
        assert!(attempt.token_matches("secret-token"));
        assert!(!attempt.token_matches("wrong-token"));
    }

    #[test]
    fn dependency_readiness_requires_every_dependency_to_succeed() {
        let mut item = work();
        item.dependencies = vec![
            WorkDependency {
                work_id: "dependency-a".into(),
                required_state: WorkState::Succeeded,
            },
            WorkDependency {
                work_id: "dependency-b".into(),
                required_state: WorkState::Succeeded,
            },
        ];
        assert!(!item.dependency_ready([WorkState::Succeeded, WorkState::Failed]));
        assert!(item.dependency_ready([WorkState::Succeeded, WorkState::Succeeded]));
    }

    #[test]
    fn terminal_states_are_not_claimable() {
        assert!(WorkState::Succeeded.is_terminal());
        assert!(WorkState::Failed.is_terminal());
        assert!(!WorkState::Leased.is_claimable());
    }
}
