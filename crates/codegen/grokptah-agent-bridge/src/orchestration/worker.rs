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

/// Public MCP observatory view of a worker. This is an allowlist: unknown
/// fields are denied on decode, and internal `WorkerProjection` fields such as
/// workspace paths, model routes, tool names, lease/attempt ids, and measured
/// provider diagnostics are not serialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerObservatoryProjection {
    pub agent_id: String,
    pub display_name: String,
    pub host_kind: WorkerHostKind,
    pub computer_use_allowed: bool,
    pub declared_tool_count: usize,
    pub load: WorkerLoad,
    pub liveness: WorkerLivenessState,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub stale_after_ms: i64,
}

impl WorkerObservatoryProjection {
    pub fn from_internal(worker: &WorkerProjection) -> Self {
        Self {
            agent_id: worker.agent_id.clone(),
            display_name: worker.display_name.clone(),
            host_kind: worker.host_kind,
            computer_use_allowed: worker.computer_use_allowed,
            declared_tool_count: worker.declared_tools.len(),
            load: worker.load.clone(),
            liveness: worker.liveness,
            last_heartbeat_at: worker.last_heartbeat_at,
            stale_after_ms: worker.stale_after_ms,
        }
    }

    pub const fn allowlisted_json_keys() -> &'static [&'static str] {
        &[
            "agentId",
            "displayName",
            "hostKind",
            "computerUseAllowed",
            "declaredToolCount",
            "load",
            "liveness",
            "lastHeartbeatAt",
            "staleAfterMs",
        ]
    }
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

    fn sensitive_internal_projection(now: DateTime<Utc>) -> WorkerProjection {
        WorkerProjection {
            agent_id: "worker-z".into(),
            display_name: "Worker Z".into(),
            workspace: "/secret/workspace".into(),
            host_kind: WorkerHostKind::Service,
            model: Some(AgentModelSpec {
                selection_key: "xai/grok-3".into(),
                provider_id: "xai".into(),
                model_id: "grok-3".into(),
            }),
            declared_tools: vec!["run_terminal_cmd".into(), "web_fetch".into()],
            measured: Some(MeasuredCapability {
                qualified_at: now,
                qualified_tools: vec!["web_fetch".into()],
                provider_route: "xai/grok-3".into(),
            }),
            policy_limits: RunBounds::default(),
            computer_use_allowed: false,
            load: WorkerLoad {
                assigned_items: 2,
                queued_items: 1,
                active_leases: 1,
            },
            liveness: WorkerLivenessState::Live,
            last_heartbeat_at: Some(now),
            stale_after_ms: DEFAULT_WORKER_STALE_AFTER_MS,
            active_leases: vec![LeaseOwnership {
                work_id: "work-1".into(),
                attempt_id: "attempt-secret".into(),
                lease_expires_at: now,
            }],
        }
    }

    #[test]
    fn observatory_projection_omits_sensitive_internal_fields() {
        let internal = sensitive_internal_projection(Utc::now());
        let public = WorkerObservatoryProjection::from_internal(&internal);
        let value = serde_json::to_value(&public).expect("serialize public worker");
        let object = value.as_object().expect("object");
        let mut keys: Vec<_> = object.keys().cloned().collect();
        keys.sort();
        let mut allowed: Vec<_> = WorkerObservatoryProjection::allowlisted_json_keys()
            .iter()
            .map(|key| (*key).to_string())
            .collect();
        allowed.sort();
        assert_eq!(keys, allowed);
        assert_eq!(public.declared_tool_count, 2);
        assert_eq!(public.load.active_leases, 1);
        let encoded = value.to_string();
        for forbidden in [
            "workspace",
            "/secret/workspace",
            "declaredTools",
            "run_terminal_cmd",
            "web_fetch",
            "measured",
            "providerRoute",
            "qualifiedTools",
            "policyLimits",
            "attempt-secret",
            "attemptId",
            "leaseExpiresAt",
            "selectionKey",
            "providerId",
            "modelId",
            "xai/grok-3",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "public worker JSON leaked {forbidden}: {encoded}"
            );
        }
        // Nested load counts keep the existing WorkerLoad field name, which is
        // a count rather than lease identity.
        assert_eq!(object["load"]["activeLeases"], 1);
        assert!(object["load"].get("attemptId").is_none());
    }

    #[test]
    fn observatory_projection_denies_unknown_fields() {
        let mut value = serde_json::to_value(WorkerObservatoryProjection::from_internal(
            &sensitive_internal_projection(Utc::now()),
        ))
        .expect("serialize");
        value["workspace"] = serde_json::json!("/secret/workspace");
        let err = serde_json::from_value::<WorkerObservatoryProjection>(value)
            .expect_err("unknown fields must be denied");
        assert!(
            err.to_string().contains("unknown field"),
            "expected deny_unknown_fields, got {err}"
        );
    }
}
