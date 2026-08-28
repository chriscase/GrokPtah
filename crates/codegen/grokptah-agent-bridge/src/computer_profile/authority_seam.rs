//! Narrow adapter seam for authority owned outside the adaptive layer.
//!
//! #477 owns host-issued principal generations, #458 owns capability
//! generations/effect leases, and #478 owns authenticated provider-attempt
//! receipts. None of those authorities exists on this exact base. Therefore
//! this module contains only erased holders and a consumer trait; it does not
//! mint identifiers, advance generations, create leases, or fabricate receipts.
//! The host stores no adapter by default and production proposal dispatch stops
//! when the adapter is absent.
#![allow(dead_code)]

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use uuid::Uuid;

const MAX_OPAQUE_REFERENCE_BYTES: usize = 128;

mod sealed {
    pub trait CanonicalAssemblyOnly {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityFailure {
    Unavailable,
    Stale,
    Revoked,
    Malformed,
    Uncertain,
}

impl fmt::Display for AuthorityFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => {
                "host-issued adaptive authority is unavailable on this assembled build"
            }
            Self::Stale => "host-issued adaptive authority is stale",
            Self::Revoked => "host-issued adaptive authority was revoked",
            Self::Malformed => "host-issued adaptive authority evidence is malformed",
            Self::Uncertain => "provider attempt outcome is uncertain",
        })
    }
}

impl std::error::Error for AuthorityFailure {}

/// Erased references returned by the future canonical authority. These values
/// are only borrowed by the adapter and never become public identifiers.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HostIssuedBinding {
    principal_generation: String,
    capability_generation: String,
    effect_lease: String,
}

impl fmt::Debug for HostIssuedBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostIssuedBinding")
            .field("principal_generation", &"[opaque]")
            .field("capability_generation", &"[opaque]")
            .field("effect_lease", &"[opaque]")
            .finish()
    }
}

impl HostIssuedBinding {
    /// Wrap values issued by the eventual #477/#458 authority. This function
    /// validates shape only; it never creates a value for a production caller.
    fn from_host_issued(
        principal_generation: String,
        capability_generation: String,
        effect_lease: String,
    ) -> Result<Self, AuthorityFailure> {
        for value in [&principal_generation, &capability_generation, &effect_lease] {
            if value.trim().is_empty()
                || value.len() > MAX_OPAQUE_REFERENCE_BYTES
                || value.contains(['\0', '/', '\\'])
            {
                return Err(AuthorityFailure::Malformed);
            }
        }
        Ok(Self {
            principal_generation,
            capability_generation,
            effect_lease,
        })
    }

    pub(crate) fn capability_reference(&self) -> &str {
        &self.capability_generation
    }

    pub(crate) fn principal_reference(&self) -> &str {
        &self.principal_generation
    }

    pub(crate) fn principal_for_request(&self) -> OpaqueAuthorityToken {
        OpaqueAuthorityToken(self.principal_generation.clone())
    }

    pub(crate) fn capability_for_request(&self) -> OpaqueAuthorityToken {
        OpaqueAuthorityToken(self.capability_generation.clone())
    }

    pub(crate) fn effect_lease_for_request(&self) -> OpaqueAuthorityToken {
        OpaqueAuthorityToken(self.effect_lease.clone())
    }
}

#[cfg(test)]
pub(crate) fn test_binding() -> HostIssuedBinding {
    HostIssuedBinding::from_host_issued(
        "test-principal-generation".into(),
        "test-capability-generation".into(),
        "test-effect-lease".into(),
    )
    .expect("test authority binding")
}

/// Token passed back to the authority adapter. Its content is not inspectable
/// by the adaptive policy and cannot be serialized by this crate.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OpaqueAuthorityToken(String);

impl fmt::Debug for OpaqueAuthorityToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueAuthorityToken([opaque])")
    }
}

/// Pre-send handle returned by the future canonical #478 transport authority.
/// It has no acknowledgment, usage, latency, or settlement data.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderAttemptHandle {
    token: OpaqueAuthorityToken,
}

impl fmt::Debug for ProviderAttemptHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAttemptHandle")
            .field("token", &"[opaque]")
            .finish()
    }
}

impl ProviderAttemptHandle {
    fn from_authority(token: String) -> Result<Self, AuthorityFailure> {
        if token.trim().is_empty()
            || token.len() > MAX_OPAQUE_REFERENCE_BYTES
            || token.contains(['\0', '/', '\\'])
        {
            return Err(AuthorityFailure::Malformed);
        }
        Ok(Self {
            token: OpaqueAuthorityToken(token),
        })
    }
}

/// Post-send authenticated evidence authored only by the future #478 physical
/// transport. It is intentionally not serializable and has no public
/// constructor.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderAttemptUsage {
    attempt_reference: String,
    provider_acknowledged: bool,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    latency_millis: u64,
}

impl fmt::Debug for ProviderAttemptUsage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAttemptUsage")
            .field("attempt_reference", &"[opaque]")
            .field("provider_acknowledged", &self.provider_acknowledged)
            .field("prompt_tokens", &self.prompt_tokens)
            .field("completion_tokens", &self.completion_tokens)
            .field("latency_millis", &self.latency_millis)
            .finish()
    }
}

impl ProviderAttemptUsage {
    fn from_transport(
        attempt_reference: String,
        provider_acknowledged: bool,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        latency_millis: u64,
    ) -> Result<Self, AuthorityFailure> {
        if attempt_reference.trim().is_empty()
            || attempt_reference.len() > MAX_OPAQUE_REFERENCE_BYTES
            || attempt_reference.contains(['\0', '/', '\\'])
            || latency_millis > 60 * 60 * 1_000
        {
            return Err(AuthorityFailure::Malformed);
        }
        Ok(Self {
            attempt_reference,
            provider_acknowledged,
            prompt_tokens,
            completion_tokens,
            latency_millis,
        })
    }

    pub(crate) fn prompt_tokens(&self) -> Option<u64> {
        self.prompt_tokens
    }

    pub(crate) fn completion_tokens(&self) -> Option<u64> {
        self.completion_tokens
    }

    pub(crate) fn latency_millis(&self) -> u64 {
        self.latency_millis
    }
}

pub(crate) struct ProviderAttemptSettlement {
    outcome: crate::computer_agent::ProposalOutcome,
    usage: ProviderAttemptUsage,
}

impl fmt::Debug for ProviderAttemptSettlement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAttemptSettlement")
            .field("outcome", &"[redacted]")
            .field("usage", &self.usage)
            .finish()
    }
}

impl ProviderAttemptSettlement {
    pub(crate) fn from_transport(
        outcome: crate::computer_agent::ProposalOutcome,
        usage: ProviderAttemptUsage,
    ) -> Self {
        Self { outcome, usage }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (crate::computer_agent::ProposalOutcome, ProviderAttemptUsage) {
        (self.outcome, self.usage)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderAttemptRequest {
    pub(crate) principal: OpaqueAuthorityToken,
    pub(crate) capability: OpaqueAuthorityToken,
    pub(crate) effect_lease: OpaqueAuthorityToken,
    pub(crate) session_id: Uuid,
    pub(crate) run_id: String,
    pub(crate) route_fingerprint: String,
    pub(crate) request_digest: String,
}

pub(crate) type ProviderInvocation<'a> = Pin<
    Box<dyn Future<Output = anyhow::Result<crate::computer_agent::ProposalOutcome>> + Send + 'a>,
>;

/// Consumer seam for the future host authority. Implementations must obtain
/// every opaque value from the canonical authority. Admission is pre-send only;
/// settlement must own the physical provider call and may author usage,
/// acknowledgment, and latency only after that call. There is intentionally no
/// synthetic implementation in production.
#[async_trait]
pub(crate) trait AdaptiveAuthorityAdapter:
    sealed::CanonicalAssemblyOnly + Send + Sync + fmt::Debug
{
    fn current_binding(
        &self,
        session_id: Uuid,
        run_id: &str,
        route_fingerprint: &str,
    ) -> Result<HostIssuedBinding, AuthorityFailure>;

    fn validate_current(&self, binding: &HostIssuedBinding) -> Result<(), AuthorityFailure>;

    fn admit_provider_attempt(
        &self,
        request: ProviderAttemptRequest,
    ) -> Result<ProviderAttemptHandle, AuthorityFailure>;

    async fn settle_provider_attempt<'a>(
        &self,
        handle: ProviderAttemptHandle,
        invocation: ProviderInvocation<'a>,
    ) -> Result<ProviderAttemptSettlement, AuthorityFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_or_path_shaped_authority_values_are_not_wrapped() {
        assert_eq!(
            HostIssuedBinding::from_host_issued(
                "principal".into(),
                "capability".into(),
                "/tmp/fake-lease".into(),
            )
            .unwrap_err(),
            AuthorityFailure::Malformed
        );
        assert!(
            ProviderAttemptUsage::from_transport("attempt".into(), false, None, None, 20,).is_ok()
        );
    }
}
