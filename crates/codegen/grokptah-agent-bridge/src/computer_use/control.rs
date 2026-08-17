//! Explicit control adapter for the MCP Computer Run mutation boundary.
//!
//! The bridge owns the durable run contract, while a desktop host owns the
//! live backend. Keeping this adapter narrow prevents the MCP plane from
//! constructing a second `ComputerUseService` against the exclusive ledger.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{
    ActionClass, ComputerAction, ComputerObservation, ComputerResult, ComputerRun,
    ComputerUseLimits,
};

/// Identity derived from the authenticated MCP transport session.
///
/// The transport session id is intentionally part of the actor identity: two
/// clients presenting the same loopback bearer token do not share a grant
/// identity merely because they use the same client name and version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerClientIdentity {
    pub transport_session_id: String,
    pub client_name: String,
    pub client_version: String,
}

impl ComputerClientIdentity {
    pub fn actor_id(&self) -> String {
        format!(
            "{}@{}#{}",
            self.client_name, self.client_version, self.transport_session_id
        )
    }
}

/// Bounded grant parameters accepted by the MCP mutation boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerGrantRequest {
    pub action_classes: BTreeSet<ActionClass>,
    pub ttl_ms: u64,
    pub uses_remaining: Option<u32>,
}

impl ComputerGrantRequest {
    pub fn validate(&self, limits: ComputerUseLimits) -> ComputerResult<()> {
        if self.action_classes.is_empty()
            || self.ttl_ms == 0
            || self.ttl_ms > limits.max_duration_secs.saturating_mul(1_000)
            || self.uses_remaining == Some(0)
            || self
                .uses_remaining
                .is_some_and(|uses| uses > limits.max_actions)
            || self
                .action_classes
                .iter()
                .any(|class| matches!(class, ActionClass::KeyChord | ActionClass::PointerFallback))
        {
            return Err(super::types::ComputerError::new(
                super::types::ComputerErrorCode::InvalidRequest,
                "invalid Computer Run grant request",
            ));
        }
        Ok(())
    }
}

/// A fresh observation plus the durable run fence that must accompany any
/// proposal derived from it.
#[derive(Debug, Clone)]
pub struct ComputerAgentObservation {
    pub observation: ComputerObservation,
    pub run_version: u64,
}

/// Live backend owner installed by the desktop host.
///
/// MCP mutation methods never open the Computer Run store themselves. The
/// registered adapter must use the desktop's already-open service so native
/// runs and simulator runs retain one ledger, one backend, and one takeover
/// fence.
#[async_trait]
pub trait ComputerRunController: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn authorize(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        grant: ComputerGrantRequest,
    ) -> ComputerResult<ComputerRun>;

    async fn pause(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun>;

    async fn take_over(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun>;

    async fn cancel(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun>;
}

/// Desktop-owned bridge for the Build agent's local Computer Use tools.
///
/// This is deliberately separate from the MCP mutation adapter: the Build
/// loop is local and can stage an approval, but it must never receive a raw
/// backend or a direct dispatch capability.
#[async_trait]
pub trait ComputerRunAgentController: Send + Sync {
    async fn observe(
        &self,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerAgentObservation>;

    async fn stage_action(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action: ComputerAction,
    ) -> ComputerResult<ComputerRun>;
}
