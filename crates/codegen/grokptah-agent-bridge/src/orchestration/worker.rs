//! Worker capability and liveness projections (#307).
//!
//! A connected MCP client is not proof of lease ownership. Liveness comes
//! from an explicit heartbeat; lease ownership is derived from Work attempts.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::types::{AgentModelSpec, AgentRecord, AgentSpec, OrchError, OrchErrorCode, RunBounds};
use super::workload::{WorkAttempt, WorkItem, WorkState};

pub const WORKER_PRESENCE_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_WORKER_STALE_AFTER_MS: i64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerHostKind {
    Desktop,
    Service,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLivenessState {
    Live,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerPresence {
    pub schema_version: u32,
    pub agent_id: String,
    pub credential_id: String,
    pub host_kind: WorkerHostKind,
    pub last_heartbeat_at: DateTime<Utc>,
}

impl WorkerPresence {
    pub fn new(
        agent_id: impl Into<String>,
        credential_id: impl Into<String>,
        host_kind: WorkerHostKind,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: WORKER_PRESENCE_SCHEMA_VERSION,
            agent_id: agent_id.into(),
            credential_id: credential_id.into(),
            host_kind,
            last_heartbeat_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasuredCapability {
    pub qualified_at: DateTime<Utc>,
    pub qualified_tools: Vec<String>,
    pub provider_route: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerLoad {
    pub assigned_items: usize,
    pub queued_items: usize,
    pub active_leases: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseOwnership {
    pub work_id: String,
    pub attempt_id: String,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerProjection {
    pub agent_id: String,
    pub display_name: String,
    pub workspace: String,
    pub host_kind: WorkerHostKind,
    pub model: Option<AgentModelSpec>,
    pub declared_tools: Vec<String>,
    pub measured: Option<MeasuredCapability>,
    pub policy_limits: RunBounds,
    pub computer_use_allowed: bool,
    pub load: WorkerLoad,
    pub liveness: WorkerLivenessState,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub stale_after_ms: i64,
    pub active_leases: Vec<LeaseOwnership>,
}

impl WorkerProjection {
    pub fn project(
        agent: &AgentRecord,
        spec: Option<&AgentSpec>,
        presence: Option<&WorkerPresence>,
        items: &[WorkItem],
        attempts: &[WorkAttempt],
        now: DateTime<Utc>,
        measured: Option<MeasuredCapability>,
    ) -> Self {
        let spec = spec.or(agent.spec.as_ref());
        let assigned: Vec<_> = items
            .iter()
            .filter(|item| item.assigned_agent_id.as_deref() == Some(agent.agent_id.as_str()))
            .collect();
        let active_leases = attempts
            .iter()
            .filter(|attempt| {
                attempt.claimant_id == agent.agent_id
                    && attempt.state.is_active()
                    && attempt.lease_active_at(now)
            })
            .map(|attempt| LeaseOwnership {
                work_id: attempt.work_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
                lease_expires_at: attempt.lease_expires_at,
            })
            .collect::<Vec<_>>();
        let liveness = match presence {
            None => WorkerLivenessState::Unknown,
            Some(presence)
                if now - presence.last_heartbeat_at
                    <= Duration::milliseconds(DEFAULT_WORKER_STALE_AFTER_MS) =>
            {
                WorkerLivenessState::Live
            }
            Some(_) => WorkerLivenessState::Stale,
        };
        Self {
            agent_id: agent.agent_id.clone(),
            display_name: spec
                .map(|spec| spec.display_name.clone())
                .unwrap_or_else(|| agent.agent_id.clone()),
            workspace: spec
                .map(|spec| spec.source_workspace.clone())
                .unwrap_or_else(|| agent.workspace.clone()),
            host_kind: presence
                .map(|presence| presence.host_kind)
                .unwrap_or(WorkerHostKind::Unknown),
            model: spec.map(|spec| spec.model.clone()),
            declared_tools: spec
                .map(|spec| spec.authority.allowed_tools.clone())
                .unwrap_or_default(),
            measured,
            policy_limits: spec
                .map(|spec| spec.default_run_bounds.clone())
                .unwrap_or_default(),
            computer_use_allowed: spec
                .map(|spec| spec.authority.computer_use_allowed)
                .unwrap_or(false),
            load: WorkerLoad {
                assigned_items: assigned.len(),
                queued_items: assigned
                    .iter()
                    .filter(|item| item.state == WorkState::Queued)
                    .count(),
                active_leases: active_leases.len(),
            },
            liveness,
            last_heartbeat_at: presence.map(|presence| presence.last_heartbeat_at),
            stale_after_ms: DEFAULT_WORKER_STALE_AFTER_MS,
            active_leases,
        }
    }
}

pub fn reject_privilege_amplification(
    manager: Option<&AgentSpec>,
    worker: &AgentSpec,
    work_bounds: &RunBounds,
    server_ceiling: &RunBounds,
) -> Result<(), OrchError> {
    if worker.authority.computer_use_allowed {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "assignment cannot select a Computer Use worker in this slice",
        ));
    }
    let manager_ceiling = manager
        .map(|spec| spec.default_run_bounds.clone())
        .unwrap_or_else(|| server_ceiling.clone());
    let ceiling = RunBounds {
        max_prompt_bytes: manager_ceiling
            .max_prompt_bytes
            .min(worker.default_run_bounds.max_prompt_bytes)
            .min(server_ceiling.max_prompt_bytes),
        max_rounds: manager_ceiling
            .max_rounds
            .min(worker.default_run_bounds.max_rounds)
            .min(server_ceiling.max_rounds),
        max_duration_ms: manager_ceiling
            .max_duration_ms
            .min(worker.default_run_bounds.max_duration_ms)
            .min(server_ceiling.max_duration_ms),
        max_total_tokens: [
            manager_ceiling.max_total_tokens,
            worker.default_run_bounds.max_total_tokens,
            server_ceiling.max_total_tokens,
        ]
        .into_iter()
        .flatten()
        .min(),
    };
    if work_bounds.max_prompt_bytes > ceiling.max_prompt_bytes
        || work_bounds.max_rounds > ceiling.max_rounds
        || work_bounds.max_duration_ms > ceiling.max_duration_ms
        || match (work_bounds.max_total_tokens, ceiling.max_total_tokens) {
            (Some(requested), Some(limit)) => requested > limit,
            _ => false,
        }
    {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "assignment would amplify bounds beyond manager, worker, or server policy",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::workload::{AttemptState, WorkAttempt};

    #[test]
    fn connected_presence_is_not_lease_ownership() {
        let now = Utc::now();
        let presence = WorkerPresence::new("agent-1", "device", WorkerHostKind::Service, now);
        let attempt = WorkAttempt {
            schema_version: 1,
            attempt_id: "att-1".into(),
            work_id: "work-1".into(),
            attempt_number: 1,
            claimant_id: "other-agent".into(),
            lease_token_hash: "hash".into(),
            acquired_at: now,
            lease_expires_at: now + Duration::minutes(5),
            last_heartbeat_at: now,
            state: AttemptState::Leased,
            linked_run_ids: Vec::new(),
            progress: None,
            result: None,
            terminal_reason: None,
            created_at: now,
            updated_at: now,
        };
        assert!(attempt.lease_active_at(now));
        assert_ne!(attempt.claimant_id, presence.agent_id);
    }

    fn bounds(
        max_prompt_bytes: usize,
        max_rounds: u32,
        max_duration_ms: u64,
        max_total_tokens: Option<u64>,
    ) -> RunBounds {
        RunBounds {
            max_prompt_bytes,
            max_rounds,
            max_duration_ms,
            max_total_tokens,
        }
    }

    fn spec(default_run_bounds: RunBounds) -> AgentSpec {
        let mut spec = AgentSpec::initial(
            "worker-1",
            "/tmp/ws",
            "grok",
            crate::orchestration::types::AgentAuthorityPolicy::default(),
            Utc::now(),
            "privilege-amplification-test",
        )
        .unwrap();
        spec.default_run_bounds = default_run_bounds;
        spec
    }

    /// Computer Use is never delegated through assignment, whatever the
    /// bounds say.
    #[test]
    fn a_computer_use_worker_is_never_assignable() {
        let server = bounds(10_000, 8, 60_000, Some(10_000));
        let mut worker = spec(server.clone());
        worker.authority.computer_use_allowed = true;
        let error = reject_privilege_amplification(None, &worker, &server, &server).unwrap_err();
        assert_eq!(error.code, OrchErrorCode::ForbiddenScope);
        assert!(
            error.message.contains("Computer Use"),
            "unexpected message: {}",
            error.message
        );

        // The same assignment is admissible once Computer Use is not claimed,
        // so the rejection above is about authority and not about bounds.
        worker.authority.computer_use_allowed = false;
        reject_privilege_amplification(None, &worker, &server, &server).unwrap();
    }

    /// Work may never request more than the narrowest of the manager, worker,
    /// and server ceilings, on every axis, whichever party is narrowest.
    #[test]
    fn work_bounds_cannot_exceed_the_narrowest_ceiling_on_any_axis() {
        let wide = bounds(10_000, 20, 600_000, Some(100_000));
        let narrow_prompt = bounds(1_000, 20, 600_000, Some(100_000));
        let narrow_rounds = bounds(10_000, 2, 600_000, Some(100_000));
        let narrow_duration = bounds(10_000, 20, 60_000, Some(100_000));
        let narrow_tokens = bounds(10_000, 20, 600_000, Some(5_000));

        // Each row narrows exactly one axis, on exactly one of the three
        // parties, and asks for one unit more than that narrowed axis allows.
        let cases: Vec<(&str, Option<RunBounds>, RunBounds, RunBounds, RunBounds)> = vec![
            (
                "manager narrows prompt bytes",
                Some(narrow_prompt.clone()),
                wide.clone(),
                wide.clone(),
                bounds(1_001, 2, 60_000, Some(5_000)),
            ),
            (
                "worker narrows rounds",
                Some(wide.clone()),
                narrow_rounds.clone(),
                wide.clone(),
                bounds(1_000, 3, 60_000, Some(5_000)),
            ),
            (
                "server narrows duration",
                Some(wide.clone()),
                wide.clone(),
                narrow_duration.clone(),
                bounds(1_000, 2, 60_001, Some(5_000)),
            ),
            (
                "worker narrows total tokens",
                Some(wide.clone()),
                narrow_tokens.clone(),
                wide.clone(),
                bounds(1_000, 2, 60_000, Some(5_001)),
            ),
        ];

        for (case, manager, worker, server, work) in cases {
            let manager_spec = manager.map(spec);
            let worker_spec = spec(worker);
            let error =
                reject_privilege_amplification(manager_spec.as_ref(), &worker_spec, &work, &server)
                    .unwrap_err();
            assert_eq!(error.code, OrchErrorCode::ForbiddenScope, "{case}");
            assert!(
                error.message.contains("amplify bounds"),
                "{case}: unexpected message {}",
                error.message
            );
        }

        // Exactly at the intersection is admissible, so the rejections above
        // are the ceiling and not an off-by-one that forbids the limit.
        let manager_spec = spec(narrow_prompt);
        let worker_spec = spec(narrow_rounds);
        reject_privilege_amplification(
            Some(&manager_spec),
            &worker_spec,
            &bounds(1_000, 2, 60_000, Some(5_000)),
            &narrow_duration,
        )
        .unwrap();
    }

    /// A manager-less assignment falls back to the server ceiling. It must not
    /// fall back to an open one.
    #[test]
    fn an_absent_manager_falls_back_to_the_server_ceiling() {
        let server = bounds(1_000, 2, 60_000, Some(5_000));
        let worker = spec(bounds(10_000, 20, 600_000, Some(100_000)));

        reject_privilege_amplification(None, &worker, &server, &server).unwrap();
        let error = reject_privilege_amplification(
            None,
            &worker,
            &bounds(1_001, 2, 60_000, Some(5_000)),
            &server,
        )
        .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::ForbiddenScope);
    }

    /// The token ceiling is the smallest limit anyone declared. A party that
    /// declares no limit does not widen the ones that did.
    #[test]
    fn the_token_ceiling_is_the_smallest_declared_limit() {
        let open = bounds(10_000, 20, 600_000, None);
        let manager = spec(open.clone());
        let worker = spec(bounds(10_000, 20, 600_000, Some(5_000)));

        // Manager and server declare no token limit; the worker's stands.
        reject_privilege_amplification(
            Some(&manager),
            &worker,
            &bounds(1_000, 2, 60_000, Some(5_000)),
            &open,
        )
        .unwrap();
        let error = reject_privilege_amplification(
            Some(&manager),
            &worker,
            &bounds(1_000, 2, 60_000, Some(5_001)),
            &open,
        )
        .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::ForbiddenScope);

        // Work that declares no token bound is not a request for an unbounded
        // Run: it simply does not override, and the effective ceiling is still
        // intersected when the Run is admitted. Pinned so a later change does
        // not turn a non-override into a rejection.
        reject_privilege_amplification(
            Some(&manager),
            &worker,
            &bounds(1_000, 2, 60_000, None),
            &open,
        )
        .unwrap();

        // Nobody declares a token limit: there is nothing to amplify past.
        let open_worker = spec(open.clone());
        reject_privilege_amplification(
            Some(&manager),
            &open_worker,
            &bounds(1_000, 2, 60_000, Some(u64::MAX)),
            &open,
        )
        .unwrap();
    }
}
