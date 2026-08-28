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

use uuid::Uuid;

const MAX_OPAQUE_REFERENCE_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityFailure {
    Unavailable,
    Stale,
    Revoked,
    Malformed,
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
    pub(crate) fn from_host_issued(
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

/// Token passed back to the authority adapter. Its content is not inspectable
/// by the adaptive policy and cannot be serialized by this crate.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OpaqueAuthorityToken(String);

impl fmt::Debug for OpaqueAuthorityToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueAuthorityToken([opaque])")
    }
}

/// Authenticated receipt returned by the future #478 physical transport.
/// Fields have no public constructor and are not deserializable.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderAttemptEvidence {
    attempt_reference: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    latency_millis: u64,
}

impl fmt::Debug for ProviderAttemptEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAttemptEvidence")
            .field("attempt_reference", &"[opaque]")
            .field("prompt_tokens", &self.prompt_tokens)
            .field("completion_tokens", &self.completion_tokens)
            .field("latency_millis", &self.latency_millis)
            .finish()
    }
}

impl ProviderAttemptEvidence {
    pub(crate) fn from_transport(
        attempt_reference: String,
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

/// Consumer seam for the future host authority. Implementations must obtain
/// every opaque value from the canonical authority and must revalidate the
/// same binding at effect time. There is intentionally no synthetic
/// implementation in production.
pub(crate) trait AdaptiveAuthorityAdapter: Send + Sync + fmt::Debug {
    fn current_binding(
        &self,
        session_id: Uuid,
        run_id: &str,
        route_fingerprint: &str,
    ) -> Result<HostIssuedBinding, AuthorityFailure>;

    fn validate_current(&self, binding: &HostIssuedBinding) -> Result<(), AuthorityFailure>;

    fn provider_attempt(
        &self,
        request: ProviderAttemptRequest,
    ) -> Result<ProviderAttemptEvidence, AuthorityFailure>;
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
        assert!(ProviderAttemptEvidence::from_transport("attempt".into(), None, None, 20,).is_ok());
    }
}
