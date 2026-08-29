use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedErrorCode, IsolatedResult};
use crate::ids::{validate_digest, validate_id, SCHEMA_VERSION};
use crate::manifest::ComputerSurfaceBinding;

pub const MAX_LEASE_LIFETIME: Duration = Duration::minutes(15);
const NORMAL_PRIORITY_AGE_WINDOW: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerSurfaceLeaseState {
    Queued,
    Granted,
    Dispatching,
    Released,
    Revoked,
    Cancelled,
    Quarantined,
    Uncertain,
}

impl ComputerSurfaceLeaseState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Released | Self::Revoked | Self::Cancelled | Self::Quarantined | Self::Uncertain
        )
    }

    pub fn owns_domain_capacity(self) -> bool {
        matches!(self, Self::Granted | Self::Dispatching)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerDispatchState {
    Prepared,
    Injected,
    Acknowledged,
    KnownNotInjected,
    Uncertain,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLeasePriority {
    #[default]
    Normal,
    OperatorUrgent,
}

impl HostLeasePriority {
    fn base_rank(self) -> u64 {
        match self {
            Self::Normal => 0,
            Self::OperatorUrgent => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerDispatchRecord {
    pub schema_version: u32,
    pub dispatch_id: String,
    pub payload_sha256: String,
    pub state: ComputerDispatchState,
    pub prepared_at: DateTime<Utc>,
    pub injected_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub outcome_sha256: Option<String>,
    pub error_code: Option<IsolatedErrorCode>,
}

impl ComputerDispatchRecord {
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid_lease_record());
        }
        validate_id("dispatch_id", &self.dispatch_id)?;
        validate_digest("payload_sha256", &self.payload_sha256)?;
        if let Some(digest) = &self.outcome_sha256 {
            validate_digest("outcome_sha256", digest)?;
        }
        let chronological = self
            .injected_at
            .is_none_or(|injected_at| injected_at >= self.prepared_at)
            && self.completed_at.is_none_or(|completed_at| {
                completed_at >= self.prepared_at
                    && self
                        .injected_at
                        .is_none_or(|injected_at| completed_at >= injected_at)
            });
        let valid = chronological
            && match self.state {
                ComputerDispatchState::Prepared => {
                    self.injected_at.is_none()
                        && self.completed_at.is_none()
                        && self.outcome_sha256.is_none()
                        && self.error_code.is_none()
                }
                ComputerDispatchState::Injected => {
                    self.injected_at.is_some()
                        && self.completed_at.is_none()
                        && self.outcome_sha256.is_none()
                        && self.error_code.is_none()
                }
                ComputerDispatchState::Acknowledged => {
                    self.injected_at.is_some()
                        && self.completed_at.is_some()
                        && self.outcome_sha256.is_some()
                        && self.error_code.is_none()
                }
                ComputerDispatchState::KnownNotInjected => {
                    self.injected_at.is_none()
                        && self.completed_at.is_some()
                        && self.outcome_sha256.is_none()
                        && self.error_code.is_some()
                }
                ComputerDispatchState::Uncertain => {
                    self.injected_at.is_some()
                        && self.completed_at.is_some()
                        && self.outcome_sha256.is_none()
                        && self.error_code.is_some()
                }
                ComputerDispatchState::Failed => {
                    self.injected_at.is_none()
                        && self.completed_at.is_some()
                        && self.outcome_sha256.is_none()
                        && self.error_code.is_some()
                }
            };
        if !valid {
            return Err(invalid_lease_record());
        }
        Ok(())
    }
}

/// Host-owned surface lease bound to exactly one Work, WorkAttempt, Agent,
/// Computer Run, surface incarnation, and conflict domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerSurfaceLease {
    pub schema_version: u32,
    pub lease_id: String,
    pub guest_id: String,
    pub work_id: String,
    pub work_attempt_id: String,
    pub agent_id: String,
    pub agent_spec_revision: u64,
    pub run_id: String,
    pub surface: ComputerSurfaceBinding,
    pub authority_epoch: u64,
    pub control_epoch: u64,
    pub frame_epoch: Option<u64>,
    pub input_domain_id: String,
    pub conflict_domain_id: String,
    pub revision: u64,
    pub expires_at: DateTime<Utc>,
    pub queue_sequence: u64,
    pub priority: HostLeasePriority,
    pub state: ComputerSurfaceLeaseState,
    pub dispatch: Option<ComputerDispatchRecord>,
    pub disposition: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ComputerSurfaceLease {
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid_lease_record());
        }
        validate_id("lease_id", &self.lease_id)?;
        validate_id("guest_id", &self.guest_id)?;
        validate_id("work_id", &self.work_id)?;
        validate_id("work_attempt_id", &self.work_attempt_id)?;
        validate_id("agent_id", &self.agent_id)?;
        validate_id("run_id", &self.run_id)?;
        validate_id("input_domain_id", &self.input_domain_id)?;
        validate_id("conflict_domain_id", &self.conflict_domain_id)?;
        self.surface.validate()?;
        if self.agent_spec_revision == 0
            || self.revision == 0
            || self.queue_sequence == 0
            || self.expires_at <= self.created_at
            || self.expires_at > self.created_at + MAX_LEASE_LIFETIME
            || self.updated_at < self.created_at
        {
            return Err(invalid_lease_record());
        }
        match (self.state, &self.dispatch) {
            (ComputerSurfaceLeaseState::Queued, None) if self.frame_epoch.is_none() => {}
            (ComputerSurfaceLeaseState::Granted, None) => {}
            (ComputerSurfaceLeaseState::Dispatching, Some(dispatch))
                if matches!(
                    dispatch.state,
                    ComputerDispatchState::Prepared | ComputerDispatchState::Injected
                ) && self.frame_epoch.is_some() => {}
            (
                ComputerSurfaceLeaseState::Released,
                Some(ComputerDispatchRecord {
                    state: ComputerDispatchState::Acknowledged,
                    ..
                }),
            ) if self.frame_epoch.is_some() => {}
            (
                ComputerSurfaceLeaseState::Uncertain,
                Some(ComputerDispatchRecord {
                    state: ComputerDispatchState::Uncertain,
                    ..
                }),
            ) if self.frame_epoch.is_some() => {}
            (
                ComputerSurfaceLeaseState::Revoked
                | ComputerSurfaceLeaseState::Cancelled
                | ComputerSurfaceLeaseState::Quarantined,
                Some(ComputerDispatchRecord {
                    state:
                        ComputerDispatchState::KnownNotInjected
                        | ComputerDispatchState::Failed
                        | ComputerDispatchState::Uncertain,
                    ..
                }),
            ) => {}
            (
                ComputerSurfaceLeaseState::Revoked
                | ComputerSurfaceLeaseState::Cancelled
                | ComputerSurfaceLeaseState::Quarantined,
                None,
            ) => {}
            _ => return Err(invalid_lease_record()),
        }
        if let Some(dispatch) = &self.dispatch {
            dispatch.validate()?;
        }
        Ok(())
    }

    pub fn effective_priority(&self, newest_sequence: u64) -> u64 {
        let age_boost = newest_sequence
            .saturating_sub(self.queue_sequence)
            .checked_div(NORMAL_PRIORITY_AGE_WINDOW)
            .unwrap_or(0)
            .min(1);
        self.priority.base_rank().saturating_add(age_boost)
    }

    pub fn transition(
        &mut self,
        next: ComputerSurfaceLeaseState,
        now: DateTime<Utc>,
        disposition: Option<&str>,
    ) -> IsolatedResult<()> {
        if now < self.updated_at {
            return Err(IsolatedError::conflict(
                "surface lease clock moved backwards",
            ));
        }
        let legal = matches!(
            (self.state, next),
            (
                ComputerSurfaceLeaseState::Queued,
                ComputerSurfaceLeaseState::Granted
            ) | (
                ComputerSurfaceLeaseState::Queued,
                ComputerSurfaceLeaseState::Cancelled
            ) | (
                ComputerSurfaceLeaseState::Queued,
                ComputerSurfaceLeaseState::Revoked
            ) | (
                ComputerSurfaceLeaseState::Queued,
                ComputerSurfaceLeaseState::Quarantined
            ) | (
                ComputerSurfaceLeaseState::Granted,
                ComputerSurfaceLeaseState::Dispatching
            ) | (
                ComputerSurfaceLeaseState::Granted,
                ComputerSurfaceLeaseState::Cancelled
            ) | (
                ComputerSurfaceLeaseState::Granted,
                ComputerSurfaceLeaseState::Revoked
            ) | (
                ComputerSurfaceLeaseState::Granted,
                ComputerSurfaceLeaseState::Quarantined
            ) | (
                ComputerSurfaceLeaseState::Dispatching,
                ComputerSurfaceLeaseState::Released
            ) | (
                ComputerSurfaceLeaseState::Dispatching,
                ComputerSurfaceLeaseState::Cancelled
            ) | (
                ComputerSurfaceLeaseState::Dispatching,
                ComputerSurfaceLeaseState::Revoked
            ) | (
                ComputerSurfaceLeaseState::Dispatching,
                ComputerSurfaceLeaseState::Quarantined
            ) | (
                ComputerSurfaceLeaseState::Dispatching,
                ComputerSurfaceLeaseState::Uncertain
            ) | (
                ComputerSurfaceLeaseState::Uncertain,
                ComputerSurfaceLeaseState::Quarantined
            )
        );
        if !legal {
            return Err(IsolatedError::invalid_state(
                "invalid surface lease transition",
            ));
        }
        self.state = next;
        self.revision = self.revision.saturating_add(1);
        self.updated_at = now;
        self.disposition = disposition.map(str::to_string);
        Ok(())
    }
}

pub fn invalid_lease_record() -> IsolatedError {
    IsolatedError::internal("computer-use surface lease record is invalid")
}

pub fn domain_has_capacity(leases: &[ComputerSurfaceLease], conflict_domain_id: &str) -> bool {
    !leases.iter().any(|lease| {
        lease.conflict_domain_id == conflict_domain_id && lease.state.owns_domain_capacity()
    })
}

pub fn attempt_has_active_lease(leases: &[ComputerSurfaceLease], work_attempt_id: &str) -> bool {
    leases
        .iter()
        .any(|lease| lease.work_attempt_id == work_attempt_id && !lease.state.is_terminal())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ComputerSurfaceBinding;

    fn lease(now: DateTime<Utc>) -> ComputerSurfaceLease {
        ComputerSurfaceLease {
            schema_version: SCHEMA_VERSION,
            lease_id: "lease-1".into(),
            guest_id: "guest-1".into(),
            work_id: "work-1".into(),
            work_attempt_id: "attempt-1".into(),
            agent_id: "agent-1".into(),
            agent_spec_revision: 1,
            run_id: "run-1".into(),
            surface: ComputerSurfaceBinding::issue(),
            authority_epoch: 1,
            control_epoch: 1,
            frame_epoch: None,
            input_domain_id: "input-1".into(),
            conflict_domain_id: "conflict-1".into(),
            revision: 1,
            expires_at: now + Duration::minutes(1),
            queue_sequence: 1,
            priority: HostLeasePriority::Normal,
            state: ComputerSurfaceLeaseState::Queued,
            dispatch: None,
            disposition: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn clock_rollback_does_not_mutate() {
        let now = Utc::now();
        let mut lease = lease(now);
        let before = lease.clone();
        let error = lease
            .transition(
                ComputerSurfaceLeaseState::Granted,
                now - Duration::seconds(1),
                None,
            )
            .unwrap_err();
        assert_eq!(error.code, IsolatedErrorCode::Conflict);
        assert_eq!(lease, before);
    }

    #[test]
    fn dispatching_without_frame_epoch_is_invalid() {
        let now = Utc::now();
        let mut lease = lease(now);
        lease.state = ComputerSurfaceLeaseState::Dispatching;
        lease.dispatch = Some(ComputerDispatchRecord {
            schema_version: SCHEMA_VERSION,
            dispatch_id: "dispatch-1".into(),
            payload_sha256: "a".repeat(64),
            state: ComputerDispatchState::Prepared,
            prepared_at: now,
            injected_at: None,
            completed_at: None,
            outcome_sha256: None,
            error_code: None,
        });
        assert!(lease.validate().is_err());
    }
}
