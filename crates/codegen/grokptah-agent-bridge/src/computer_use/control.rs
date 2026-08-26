//! Explicit control adapter for the MCP Computer Run mutation boundary.
//!
//! The bridge owns the durable run contract, while a desktop host owns the
//! live backend. Keeping this adapter narrow prevents the MCP plane from
//! constructing a second `ComputerUseService` against the exclusive ledger.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::approval::{ApprovalPresentation, ApprovalPrincipal, ApprovalProjection};
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

    /// Combine transport identity with the control-plane principal so a
    /// human approval receipt is bound to *both*.
    ///
    /// Neither half is sufficient on its own: the bearer token says which
    /// control plane is calling, the transport session says which client
    /// instance is calling, and only the pair identifies the caller a human
    /// actually approved.
    pub fn approval_principal(
        &self,
        principal_id: &str,
        token_fingerprint: &str,
    ) -> ApprovalPrincipal {
        ApprovalPrincipal {
            principal_id: principal_id.to_owned(),
            token_fingerprint: token_fingerprint.to_owned(),
            mcp_session_id: self.transport_session_id.clone(),
            client_actor_id: self.actor_id(),
        }
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

/// A staged approval request plus the one-time nonce handed to the requester.
///
/// The projection is share-safe; the nonce is not. Keep them separate at
/// every boundary so a redacted read path can never accidentally serve the
/// secret.
#[derive(Debug, Clone)]
pub struct IssuedApproval {
    /// Redaction-safe record view.
    pub approval: ApprovalProjection,
    /// One-time secret. Returned to the requester exactly once.
    pub nonce: String,
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
    /// Stage a human-approval request for one exact control intent.
    ///
    /// This is the *only* way an MCP caller reaches the `computer.control`
    /// gate, and it grants nothing on its own. The returned nonce is the
    /// caller's one-time proof that it is the same requester; it is issued
    /// once and never re-served.
    #[allow(clippy::too_many_arguments)]
    async fn request_approval(
        &self,
        principal: &ApprovalPrincipal,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        capability_revision: &str,
        grant: ComputerGrantRequest,
    ) -> ComputerResult<IssuedApproval>;

    /// Read one approval record the caller itself requested.
    async fn read_approval(
        &self,
        principal: &ApprovalPrincipal,
        owner_session_id: Uuid,
        workspace: &str,
        approval_id: &str,
    ) -> ComputerResult<ApprovalProjection>;

    /// Attach control authority by spending a host-issued approval receipt.
    ///
    /// The receipt is required. A caller-supplied Boolean, an authenticated
    /// bearer token, and an initialized MCP session are each insufficient,
    /// individually and together.
    #[allow(clippy::too_many_arguments)]
    async fn authorize(
        &self,
        principal: &ApprovalPrincipal,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        capability_revision: &str,
        grant: ComputerGrantRequest,
        presentation: &ApprovalPresentation,
    ) -> ComputerResult<ComputerRun>;

    /// De-escalating safety controls.
    ///
    /// `pause`, `take_over`, and `cancel` deliberately do **not** take an
    /// approval receipt. They only ever reduce agent authority, and requiring
    /// a fresh human decision to stop a running agent would be a safety
    /// regression, not a hardening. They keep the plain transport identity and
    /// the existing lease / stale-observation fences.
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
