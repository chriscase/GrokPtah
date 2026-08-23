//! Durable host-owned Computer Use surface coordination.
//!
//! This module does not schedule Agent work. It fences one already-admitted
//! Computer Run/WorkAttempt onto one host-attested physical input conflict
//! domain. A backend or caller never chooses a surface, conflict domain, queue
//! sequence, lease revision, or physical dispatch state.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::types::{
    validate_id, ComputerError, ComputerErrorCode, ComputerResult, ComputerRun,
    ComputerSurfaceBinding,
};

pub(crate) const COMPUTER_SURFACE_LEASE_SCHEMA_VERSION: u32 = 1;
pub(crate) const COMPUTER_DISPATCH_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_SURFACE_LEASES: usize = 512;
pub(crate) const MAX_LEASE_LIFETIME: Duration = Duration::minutes(15);
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

    pub(crate) fn owns_domain_capacity(self) -> bool {
        matches!(self, Self::Granted | Self::Dispatching)
    }

    /// Ordinary terminal records may be retired once their independent
    /// mutation receipt carries the replay result. An uncertain physical
    /// dispatch is deliberately excluded: deleting it would erase the
    /// durable no-replay fence after the backend injection boundary.
    pub(crate) fn is_retention_prunable(self) -> bool {
        matches!(
            self,
            Self::Released | Self::Revoked | Self::Cancelled | Self::Quarantined
        )
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

/// Priority is derived by trusted Work admission. Callers and backends never
/// provide this enum on a wire surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostLeasePriority {
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

/// Host-only queue input. Surface, principal, epochs, queue sequence, and
/// conflict-domain identity are derived from the Run and live host registry.
#[derive(Debug, Clone)]
pub(crate) struct HostSurfaceLeaseRequest {
    pub work_id: String,
    pub work_attempt_id: String,
    pub run_id: String,
    pub priority: HostLeasePriority,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ComputerDispatchClaim {
    Perform(ComputerSurfaceLease),
    Replay(ComputerSurfaceLease),
    Pending,
    Uncertain,
}

impl HostSurfaceLeaseRequest {
    pub(crate) fn validate(&self, now: DateTime<Utc>) -> ComputerResult<()> {
        validate_id("work_id", &self.work_id)?;
        validate_id("work_attempt_id", &self.work_attempt_id)?;
        validate_id("run_id", &self.run_id)?;
        if self.expires_at <= now || self.expires_at > now + MAX_LEASE_LIFETIME {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "surface lease expiry is outside the host-owned lifetime ceiling",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ComputerDispatchRecord {
    pub schema_version: u32,
    pub dispatch_id: String,
    pub payload_sha256: String,
    pub state: ComputerDispatchState,
    pub prepared_at: DateTime<Utc>,
    pub injected_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub outcome_sha256: Option<String>,
    pub error_code: Option<ComputerErrorCode>,
}

impl ComputerDispatchRecord {
    pub(crate) fn validate(&self) -> ComputerResult<()> {
        if self.schema_version != COMPUTER_DISPATCH_SCHEMA_VERSION {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ComputerSurfaceLease {
    pub schema_version: u32,
    pub lease_id: String,
    pub work_id: String,
    pub work_attempt_id: String,
    pub agent_id: String,
    pub agent_spec_revision: u64,
    pub run_id: String,
    pub surface: ComputerSurfaceBinding,
    pub authority_epoch: u64,
    pub control_epoch: u64,
    /// Bound only after the granted Agent captures a host-stamped frame.
    /// Queued and not-yet-observed Granted leases carry `None`.
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
    pub(crate) fn validate(&self) -> ComputerResult<()> {
        if self.schema_version != COMPUTER_SURFACE_LEASE_SCHEMA_VERSION {
            return Err(invalid_lease_record());
        }
        validate_id("lease_id", &self.lease_id)?;
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
        if self
            .disposition
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 128 || value.contains('\0'))
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
                    state: ComputerDispatchState::KnownNotInjected | ComputerDispatchState::Failed,
                    ..
                }),
            ) if self.frame_epoch.is_some() => {}
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
            if dispatch.prepared_at < self.created_at
                || dispatch.prepared_at > self.updated_at
                || dispatch
                    .injected_at
                    .is_some_and(|injected_at| injected_at > self.updated_at)
                || dispatch
                    .completed_at
                    .is_some_and(|completed_at| completed_at > self.updated_at)
            {
                return Err(invalid_lease_record());
            }
        }
        Ok(())
    }

    pub(crate) fn assert_run_fence(&self, run: &ComputerRun) -> ComputerResult<()> {
        let principal = run.required_principal()?;
        if run.run_id != self.run_id
            || run.surface != self.surface
            || run.authority_epoch != self.authority_epoch
            || run.control_epoch != self.control_epoch
            || principal.agent_id() != Some(self.agent_id.as_str())
            || principal.agent_spec_revision() != Some(self.agent_spec_revision)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "surface lease no longer matches its host-issued run fence",
            ));
        }
        if let Some(frame_epoch) = self.frame_epoch {
            if run
                .current_observation
                .as_ref()
                .map(|observation| observation.authority.frame_epoch)
                != Some(frame_epoch)
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "surface lease no longer matches its host-stamped frame",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn effective_priority(&self, newest_sequence: u64) -> u64 {
        let age_boost = newest_sequence
            .saturating_sub(self.queue_sequence)
            .checked_div(NORMAL_PRIORITY_AGE_WINDOW)
            .unwrap_or(0)
            .min(1);
        self.priority.base_rank().saturating_add(age_boost)
    }

    pub(crate) fn transition(
        &mut self,
        next: ComputerSurfaceLeaseState,
        now: DateTime<Utc>,
        disposition: Option<&str>,
    ) -> ComputerResult<()> {
        if now < self.updated_at {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
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
            )
        );
        if !legal {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
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

pub(crate) fn conflict_domain_id(physical_domain_key: &str) -> String {
    let digest = Sha256::digest(
        [
            b"grokptah-computer-conflict-v1\0".as_slice(),
            physical_domain_key.as_bytes(),
        ]
        .concat(),
    );
    format!("conflict-{:x}", digest)
}

pub(crate) fn validate_payload_digest(value: &str) -> ComputerResult<()> {
    validate_digest("payload_sha256", value)
}

fn validate_digest(name: &str, value: &str) -> ComputerResult<()> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            format!("{name} must be a lowercase SHA-256 digest"),
        ));
    }
    Ok(())
}

pub(crate) fn invalid_lease_record() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::Internal,
        "computer-use surface lease record is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_priority_ages_to_prevent_starvation() {
        let now = Utc::now();
        let lease = ComputerSurfaceLease {
            schema_version: COMPUTER_SURFACE_LEASE_SCHEMA_VERSION,
            lease_id: "lease-1".into(),
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
        };
        assert_eq!(lease.effective_priority(1), 0);
        assert_eq!(lease.effective_priority(8), 0);
        assert_eq!(lease.effective_priority(9), 1);
        assert_eq!(lease.effective_priority(100), 1);
    }

    #[test]
    fn lease_transition_rejects_clock_rollback_without_mutation() {
        let now = Utc::now();
        let mut lease = ComputerSurfaceLease {
            schema_version: COMPUTER_SURFACE_LEASE_SCHEMA_VERSION,
            lease_id: "lease-clock".into(),
            work_id: "work-clock".into(),
            work_attempt_id: "attempt-clock".into(),
            agent_id: "agent-clock".into(),
            agent_spec_revision: 1,
            run_id: "run-clock".into(),
            surface: ComputerSurfaceBinding::issue(),
            authority_epoch: 1,
            control_epoch: 1,
            frame_epoch: None,
            input_domain_id: "input-clock".into(),
            conflict_domain_id: "conflict-clock".into(),
            revision: 1,
            expires_at: now + Duration::minutes(1),
            queue_sequence: 1,
            priority: HostLeasePriority::Normal,
            state: ComputerSurfaceLeaseState::Queued,
            dispatch: None,
            disposition: None,
            created_at: now,
            updated_at: now,
        };
        let before = lease.clone();
        let error = lease
            .transition(
                ComputerSurfaceLeaseState::Granted,
                now - Duration::seconds(1),
                None,
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Conflict);
        assert_eq!(lease, before);
    }

    #[test]
    fn dispatch_shape_is_closed_and_state_dependent() {
        let now = Utc::now();
        let mut dispatch = ComputerDispatchRecord {
            schema_version: COMPUTER_DISPATCH_SCHEMA_VERSION,
            dispatch_id: "dispatch-1".into(),
            payload_sha256: "a".repeat(64),
            state: ComputerDispatchState::Prepared,
            prepared_at: now,
            injected_at: None,
            completed_at: None,
            outcome_sha256: None,
            error_code: None,
        };
        dispatch.validate().unwrap();
        dispatch.state = ComputerDispatchState::Injected;
        assert!(dispatch.validate().is_err());
        dispatch.injected_at = Some(now);
        dispatch.validate().unwrap();
        dispatch.state = ComputerDispatchState::Acknowledged;
        dispatch.completed_at = Some(now);
        dispatch.outcome_sha256 = Some("b".repeat(64));
        dispatch.validate().unwrap();
    }

    #[test]
    fn lease_state_requires_exact_physical_dispatch_evidence() {
        let now = Utc::now();
        let mut lease = ComputerSurfaceLease {
            schema_version: COMPUTER_SURFACE_LEASE_SCHEMA_VERSION,
            lease_id: "lease-shape".into(),
            work_id: "work-shape".into(),
            work_attempt_id: "attempt-shape".into(),
            agent_id: "agent-shape".into(),
            agent_spec_revision: 1,
            run_id: "run-shape".into(),
            surface: ComputerSurfaceBinding::issue(),
            authority_epoch: 1,
            control_epoch: 1,
            frame_epoch: None,
            input_domain_id: "input-shape".into(),
            conflict_domain_id: "conflict-shape".into(),
            revision: 1,
            expires_at: now + Duration::minutes(1),
            queue_sequence: 1,
            priority: HostLeasePriority::Normal,
            state: ComputerSurfaceLeaseState::Queued,
            dispatch: None,
            disposition: None,
            created_at: now,
            updated_at: now,
        };
        lease.validate().unwrap();

        lease.state = ComputerSurfaceLeaseState::Uncertain;
        assert!(lease.validate().is_err());

        lease.state = ComputerSurfaceLeaseState::Released;
        lease.frame_epoch = Some(1);
        lease.dispatch = Some(ComputerDispatchRecord {
            schema_version: COMPUTER_DISPATCH_SCHEMA_VERSION,
            dispatch_id: "dispatch-shape".into(),
            payload_sha256: "a".repeat(64),
            state: ComputerDispatchState::Acknowledged,
            prepared_at: now,
            injected_at: Some(now),
            completed_at: Some(now),
            outcome_sha256: Some("b".repeat(64)),
            error_code: None,
        });
        lease.validate().unwrap();

        lease.state = ComputerSurfaceLeaseState::Uncertain;
        assert!(lease.validate().is_err());
        lease.state = ComputerSurfaceLeaseState::Released;
        lease.expires_at = now + MAX_LEASE_LIFETIME + Duration::seconds(1);
        assert!(lease.validate().is_err());
    }
}
