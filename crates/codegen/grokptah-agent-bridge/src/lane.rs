//! Product-facing Lane projections.
//!
//! Sessions remain the persisted compatibility primitive for now. A Lane is
//! the user-facing work context projected from a session, which lets the
//! desktop and service surfaces adopt Agent/Lane terminology without
//! rewriting the existing session store in one step.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::{SessionKind, SessionSummary, WorkspaceStatus};
use crate::RunExecutionMode;

/// Where a Lane's next Run is expected to execute.
///
/// The first projection is local-desktop because that is the current owner of
/// persisted sessions. Service-owned lanes can be introduced without changing
/// the Lane identity or the desktop's active-context contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTarget {
    LocalDesktop,
    LocalService,
    HostedService,
}

impl Default for RuntimeTarget {
    fn default() -> Self {
        Self::LocalDesktop
    }
}

/// Connection state for the runtime that owns a Lane.
///
/// This is deliberately separate from `RuntimeTarget`: a hosted Lane can be
/// disconnected without changing ownership, and a reconnecting client must
/// not present that Lane as local work merely because the transport is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConnectionState {
    Connected,
    Reconnecting,
    Disconnected,
    Error,
}

impl Default for RuntimeConnectionState {
    fn default() -> Self {
        Self::Connected
    }
}

/// Stable user-facing work context, initially backed 1:1 by a Session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneSummary {
    /// Lane identity. During compatibility projection this equals `session_id`.
    pub id: Uuid,
    /// Persisted session backing this Lane during the migration period.
    pub session_id: Uuid,
    #[serde(default)]
    pub agent_id: Option<String>,
    pub title: String,
    pub cwd: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub message_count: usize,
    #[serde(default)]
    pub archived: bool,
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub kind: SessionKind,
    #[serde(default)]
    pub execution_mode: RunExecutionMode,
    #[serde(default)]
    pub workspace_status: WorkspaceStatus,
    #[serde(default)]
    pub runtime_target: RuntimeTarget,
    #[serde(default)]
    pub runtime_connection: RuntimeConnectionState,
}

impl From<&SessionSummary> for LaneSummary {
    fn from(session: &SessionSummary) -> Self {
        Self {
            id: session.id,
            session_id: session.id,
            agent_id: session.agent_id.clone(),
            title: session.title.clone(),
            cwd: session.cwd.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            message_count: session.message_count,
            archived: session.archived,
            archived_at: session.archived_at,
            kind: session.kind,
            execution_mode: session.execution_mode,
            workspace_status: session.workspace_status,
            runtime_target: RuntimeTarget::LocalDesktop,
            runtime_connection: RuntimeConnectionState::Connected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_a_legacy_session_as_a_local_desktop_lane() {
        let session = SessionSummary {
            id: Uuid::new_v4(),
            agent_id: Some("agent-stable".into()),
            title: "Implement lanes".into(),
            cwd: "/tmp/project".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            message_count: 3,
            forked_from: None,
            folder: None,
            tags: Vec::new(),
            archived: false,
            archived_at: None,
            kind: SessionKind::Build,
            execution_mode: RunExecutionMode::Shared,
            workspace_status: WorkspaceStatus::Ready,
        };

        let lane = LaneSummary::from(&session);
        assert_eq!(lane.id, session.id);
        assert_eq!(lane.session_id, session.id);
        assert_eq!(lane.agent_id.as_deref(), Some("agent-stable"));
        assert_eq!(lane.runtime_target, RuntimeTarget::LocalDesktop);
        assert_eq!(lane.runtime_connection, RuntimeConnectionState::Connected);
    }

    #[test]
    fn runtime_target_round_trips_as_stable_wire_values() {
        let encoded = serde_json::to_string(&RuntimeTarget::HostedService).unwrap();
        assert_eq!(encoded, "\"hosted_service\"");
        let decoded: RuntimeTarget = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, RuntimeTarget::HostedService);
    }

    #[test]
    fn old_lane_payloads_default_to_a_connected_runtime() {
        let value = serde_json::json!({
            "id": Uuid::new_v4(),
            "session_id": Uuid::new_v4(),
            "title": "legacy",
            "cwd": "/tmp/project",
            "created_at": chrono::Utc::now(),
            "updated_at": chrono::Utc::now(),
            "message_count": 0,
            "archived_at": null
        });
        let lane: LaneSummary = serde_json::from_value(value).unwrap();
        assert_eq!(lane.runtime_target, RuntimeTarget::LocalDesktop);
        assert_eq!(lane.runtime_connection, RuntimeConnectionState::Connected);
    }

    #[test]
    fn runtime_connection_round_trips_as_stable_wire_values() {
        let encoded = serde_json::to_string(&RuntimeConnectionState::Reconnecting).unwrap();
        assert_eq!(encoded, "\"reconnecting\"");
        let decoded: RuntimeConnectionState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, RuntimeConnectionState::Reconnecting);
    }
}
