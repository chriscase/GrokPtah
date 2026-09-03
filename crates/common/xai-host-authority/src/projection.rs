//! Secret-free read projections for public and broker surfaces.
//!
//! These types are the only authority identity shapes meant to cross an SDK or
//! MCP boundary. They carry no bearer material, no filesystem paths, and no
//! host-native handles that could be replayed as authority.

use serde::Serialize;

use crate::receipt::AuthContext;

/// Read-only principal and generation binding for external APIs.
///
/// Minted only from a current [`AuthContext`] after the host re-validates every
/// generation field. Callers cannot construct one with fabricated owner,
/// credential, tenant, session, workspace, role, or generation values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalProjection {
    /// Opaque, non-reversible principal handle.
    pub principal: String,
    /// Stable credential slot identity (not the secret).
    pub credential_id: String,
    /// Owner/tenant account this principal belongs to.
    pub owner_id: String,
    /// Authentication generation at issue time.
    pub auth_generation: u64,
    /// Capability generation at issue time.
    pub capability_generation: u64,
    /// Host policy revision at issue time.
    pub policy_revision: u64,
    /// Control-plane epoch at issue time.
    pub control_epoch: u64,
}

impl PrincipalProjection {
    pub(crate) fn from_context(auth: &AuthContext) -> Self {
        Self {
            principal: auth.public_handle(),
            credential_id: auth.credential_id().to_string(),
            owner_id: auth.owner_id().to_string(),
            auth_generation: auth.auth_generation().raw(),
            capability_generation: auth.capability_generation().raw(),
            policy_revision: auth.policy_revision().raw(),
            control_epoch: auth.control_epoch().raw(),
        }
    }
}

/// Non-authoritative liveness snapshot for readiness/health probes.
///
/// This deliberately omits principal identity and cannot be converted into an
/// [`AuthContext`]. Health paths must never mint caller authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceLivenessProjection {
    pub schema_version: u32,
    pub credentials_configured: bool,
    pub control_epoch: u64,
    pub policy_revision: u64,
    pub capability_generation: u64,
}
