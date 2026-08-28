//! Interfaces to the canonical authority spine owned by #477, #458, and #478.
//!
//! This module deliberately contains no authority implementation. The adaptive
//! layer consumes host-issued opaque references; it never mints principals,
//! capability generations, provider receipts, or replacement leases. Until the
//! assembled host supplies this interface, live adaptive proposals fail closed.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_REFERENCE_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityFailure {
    Unavailable,
    StaleGeneration,
    Revoked,
    InvalidReceipt,
}

impl fmt::Display for AuthorityFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "canonical Computer Use authority is unavailable",
            Self::StaleGeneration => "canonical Computer Use authority generation is stale",
            Self::Revoked => "canonical Computer Use authority was revoked",
            Self::InvalidReceipt => "canonical provider attempt receipt is invalid",
        })
    }
}

impl std::error::Error for AuthorityFailure {}

/// Host-issued opaque principal generation from #477.
///
/// There is no public constructor. A future canonical authority implementation
/// creates this value after authenticating host state.
#[derive(Clone, PartialEq, Eq)]
pub struct PrincipalGenerationRef {
    value: String,
}

impl fmt::Debug for PrincipalGenerationRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrincipalGenerationRef([opaque])")
    }
}

impl PrincipalGenerationRef {
    pub(crate) fn issued(value: impl Into<String>) -> Result<Self, AuthorityFailure> {
        let value = value.into();
        validate_reference(&value)?;
        Ok(Self { value })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.value
    }
}

/// Host-issued capability generation/snapshot from #458.
#[derive(Clone, PartialEq, Eq)]
pub struct CapabilityGenerationRef {
    value: String,
}

impl fmt::Debug for CapabilityGenerationRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CapabilityGenerationRef([opaque])")
    }
}

impl CapabilityGenerationRef {
    pub(crate) fn issued(value: impl Into<String>) -> Result<Self, AuthorityFailure> {
        let value = value.into();
        validate_reference(&value)?;
        Ok(Self { value })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.value
    }
}

/// Authenticated provider-attempt receipt from #478. The adaptive layer may
/// record usage only from this receipt; it never estimates or derives cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttemptReceipt {
    pub attempt_id: String,
    pub provider_acknowledged: bool,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub latency_millis: u64,
}

impl ProviderAttemptReceipt {
    pub(crate) fn validate(&self) -> Result<(), AuthorityFailure> {
        validate_reference(&self.attempt_id)?;
        if self.latency_millis > 60 * 60 * 1_000 {
            return Err(AuthorityFailure::InvalidReceipt);
        }
        Ok(())
    }
}

/// The generation references that an adaptive decision must carry.
#[derive(Clone, PartialEq, Eq)]
pub struct AdaptiveAuthoritySnapshot {
    principal: PrincipalGenerationRef,
    capability: CapabilityGenerationRef,
}

impl fmt::Debug for AdaptiveAuthoritySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveAuthoritySnapshot")
            .field("principal", &"[opaque]")
            .field("capability", &"[opaque]")
            .finish()
    }
}

impl AdaptiveAuthoritySnapshot {
    pub(crate) fn issued(
        principal: impl Into<String>,
        capability: impl Into<String>,
    ) -> Result<Self, AuthorityFailure> {
        Ok(Self {
            principal: PrincipalGenerationRef::issued(principal)?,
            capability: CapabilityGenerationRef::issued(capability)?,
        })
    }

    pub(crate) fn capability_reference(&self) -> &str {
        self.capability.as_str()
    }

    pub(crate) fn principal_reference(&self) -> &str {
        self.principal.as_str()
    }

    pub(crate) fn principal(&self) -> PrincipalGenerationRef {
        self.principal.clone()
    }

    pub(crate) fn capability(&self) -> CapabilityGenerationRef {
        self.capability.clone()
    }
}

/// Request passed to the canonical #478 provider-attempt authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAttemptRequest {
    pub principal: PrincipalGenerationRef,
    pub capability: CapabilityGenerationRef,
    pub session_id: Uuid,
    pub run_id: String,
    pub route_fingerprint: String,
    pub request_digest: String,
}

/// Adapter boundary for the assembled host's canonical authority.
///
/// Implementations must validate the same generation again at effect time.
/// Implementations must also make a provider attempt receipt authoritative at
/// the physical provider write boundary. No implementation is bundled here:
/// a missing adapter is an intentional fail-closed state.
pub trait CanonicalAuthority: Send + Sync + fmt::Debug {
    fn current_snapshot(
        &self,
        session_id: Uuid,
        run_id: &str,
        route_fingerprint: &str,
    ) -> Result<AdaptiveAuthoritySnapshot, AuthorityFailure>;

    fn validate_snapshot(
        &self,
        snapshot: &AdaptiveAuthoritySnapshot,
    ) -> Result<(), AuthorityFailure>;

    fn provider_attempt(
        &self,
        request: ProviderAttemptRequest,
    ) -> Result<ProviderAttemptReceipt, AuthorityFailure>;
}

fn validate_reference(value: &str) -> Result<(), AuthorityFailure> {
    if value.trim().is_empty()
        || value.len() > MAX_REFERENCE_BYTES
        || value.contains(['\0', '/', '\\'])
    {
        return Err(AuthorityFailure::InvalidReceipt);
    }
    Ok(())
}
