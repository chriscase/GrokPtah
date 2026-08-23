//! Fail-closed admission for the enterprise gateway review lane.
//!
//! The lease is intentionally an opaque, secret-free handoff between an
//! operator-owned gateway broker and GrokPtah orchestration. It describes the
//! exact route/model binding and the bounded read-only policy, but never carries
//! a bearer, URL, API key, or provider response. A live campaign still needs
//! an operator-owned broker and an external egress attestation; this module is
//! the product-side boundary that refuses to run without both.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ENTERPRISE_REVIEW_LEASE_SCHEMA: &str = "grokptah.enterprise-review-lease.v1";
pub const ENTERPRISE_REVIEW_ATTESTATION_SCHEMA: &str = "grokptah.enterprise-gateway-attestation.v1";
pub const ENTERPRISE_REVIEW_EVIDENCE_SCHEMA: &str = "grokptah.enterprise-review-evidence.v1";
pub const MAX_ENTERPRISE_REVIEW_REQUESTS: u32 = 400;
pub const MAX_ENTERPRISE_REVIEW_TOKENS: u64 = 1_250_000;
pub const MAX_ENTERPRISE_REVIEW_DURATION_MS: u64 = 8 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnterpriseModelTier {
    Modest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseGatewayAttestation {
    pub schema: String,
    pub route_id: String,
    pub endpoint_fingerprint: String,
    pub model_id: String,
    pub model_tier: EnterpriseModelTier,
    pub deployment_revision: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub no_premium_fallback: bool,
    pub egress_firewall_attested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewLease {
    pub schema: String,
    pub lease_id: String,
    pub credential_id: String,
    pub route_id: String,
    pub endpoint_fingerprint: String,
    pub model_id: String,
    pub model_tier: EnterpriseModelTier,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub route_binding_digest: String,
    pub read_only: bool,
    pub allow_network: bool,
    pub allow_workspace_writes: bool,
    pub allow_publication: bool,
    pub max_requests: u32,
    pub max_tokens: u64,
    pub max_duration_ms: u64,
    pub attestation: EnterpriseGatewayAttestation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewPolicy {
    pub max_requests: u32,
    pub max_tokens: u64,
    pub max_duration_ms: u64,
    pub read_only: bool,
    pub allow_network: bool,
    pub allow_workspace_writes: bool,
    pub allow_publication: bool,
}

impl Default for EnterpriseReviewPolicy {
    fn default() -> Self {
        Self {
            max_requests: MAX_ENTERPRISE_REVIEW_REQUESTS,
            max_tokens: MAX_ENTERPRISE_REVIEW_TOKENS,
            max_duration_ms: MAX_ENTERPRISE_REVIEW_DURATION_MS,
            read_only: true,
            allow_network: false,
            allow_workspace_writes: false,
            allow_publication: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewEvidence {
    pub schema: String,
    pub lease_id: String,
    pub route_id: String,
    pub model_id: String,
    pub model_tier: EnterpriseModelTier,
    pub route_binding_digest: String,
    pub policy_digest: String,
    pub read_only: bool,
    pub no_premium_fallback: bool,
    pub egress_firewall_attested: bool,
    pub secret_free: bool,
}

impl EnterpriseReviewEvidence {
    /// Validate evidence after it has crossed a durable or transport boundary.
    /// This checks the safe admission projection, not the original lease
    /// validity window; a host must still re-admit a fresh lease before a new
    /// review is started.
    pub fn validate(&self) -> Result<(), EnterpriseReviewAdmissionError> {
        if self.schema != ENTERPRISE_REVIEW_EVIDENCE_SCHEMA {
            return Err(EnterpriseReviewAdmissionError::UnsupportedSchema);
        }
        for (value, name) in [
            (&self.lease_id, "lease_id"),
            (&self.route_id, "route_id"),
            (&self.model_id, "model_id"),
        ] {
            if !valid_opaque_id(value) {
                return Err(EnterpriseReviewAdmissionError::InvalidField(name));
            }
        }
        if !valid_fingerprint(&self.route_binding_digest) || !valid_fingerprint(&self.policy_digest)
        {
            return Err(EnterpriseReviewAdmissionError::InvalidField("fingerprint"));
        }
        if !self.read_only {
            return Err(EnterpriseReviewAdmissionError::ReviewMustBeReadOnly);
        }
        if !self.no_premium_fallback {
            return Err(EnterpriseReviewAdmissionError::FallbackPermitted);
        }
        if !self.egress_firewall_attested {
            return Err(EnterpriseReviewAdmissionError::EgressFirewallMissing);
        }
        if !self.secret_free {
            return Err(EnterpriseReviewAdmissionError::InvalidField("secret_free"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterpriseReviewAdmissionError {
    InvalidField(&'static str),
    UnsupportedSchema,
    NotYetValid,
    Expired,
    AttestationMismatch(&'static str),
    FallbackPermitted,
    EgressFirewallMissing,
    ReviewMustBeReadOnly,
    NetworkNotAllowed,
    WorkspaceWritesNotAllowed,
    PublicationNotAllowed,
    BoundExceeded(&'static str),
    RouteBindingMismatch,
}

impl std::fmt::Display for EnterpriseReviewAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(name) => write!(f, "invalid enterprise review field: {name}"),
            Self::UnsupportedSchema => write!(f, "unsupported enterprise review schema"),
            Self::NotYetValid => write!(f, "enterprise review lease is not yet valid"),
            Self::Expired => write!(f, "enterprise review lease is expired"),
            Self::AttestationMismatch(name) => write!(f, "gateway attestation mismatch: {name}"),
            Self::FallbackPermitted => write!(f, "premium fallback is not permitted"),
            Self::EgressFirewallMissing => write!(f, "external egress attestation is missing"),
            Self::ReviewMustBeReadOnly => write!(f, "enterprise review must be read-only"),
            Self::NetworkNotAllowed => write!(f, "network access is not allowed for review"),
            Self::WorkspaceWritesNotAllowed => {
                write!(f, "workspace writes are not allowed for review")
            }
            Self::PublicationNotAllowed => write!(f, "publication is not allowed for review"),
            Self::BoundExceeded(name) => write!(f, "enterprise review bound exceeded: {name}"),
            Self::RouteBindingMismatch => write!(f, "enterprise route binding digest mismatch"),
        }
    }
}

impl std::error::Error for EnterpriseReviewAdmissionError {}

/// Validate a broker-issued lease and return only safe evidence for the public
/// report. No secret or endpoint URL crosses this boundary.
pub fn admit_enterprise_review(
    lease: &EnterpriseReviewLease,
    policy: &EnterpriseReviewPolicy,
    now: DateTime<Utc>,
) -> Result<EnterpriseReviewEvidence, EnterpriseReviewAdmissionError> {
    if lease.schema != ENTERPRISE_REVIEW_LEASE_SCHEMA
        || lease.attestation.schema != ENTERPRISE_REVIEW_ATTESTATION_SCHEMA
    {
        return Err(EnterpriseReviewAdmissionError::UnsupportedSchema);
    }
    for (value, name) in [
        (&lease.lease_id, "lease_id"),
        (&lease.credential_id, "credential_id"),
        (&lease.route_id, "route_id"),
        (&lease.model_id, "model_id"),
        (
            &lease.attestation.deployment_revision,
            "deployment_revision",
        ),
    ] {
        if !valid_opaque_id(value) {
            return Err(EnterpriseReviewAdmissionError::InvalidField(name));
        }
    }
    if !valid_fingerprint(&lease.endpoint_fingerprint)
        || !valid_fingerprint(&lease.route_binding_digest)
    {
        return Err(EnterpriseReviewAdmissionError::InvalidField("fingerprint"));
    }
    if lease.issued_at > lease.expires_at
        || lease.attestation.issued_at > lease.attestation.expires_at
    {
        return Err(EnterpriseReviewAdmissionError::InvalidField("validity"));
    }
    if now < lease.issued_at || now < lease.attestation.issued_at {
        return Err(EnterpriseReviewAdmissionError::NotYetValid);
    }
    if now >= lease.expires_at || now >= lease.attestation.expires_at {
        return Err(EnterpriseReviewAdmissionError::Expired);
    }
    for (matches, name) in [
        (lease.route_id == lease.attestation.route_id, "route_id"),
        (
            lease.endpoint_fingerprint == lease.attestation.endpoint_fingerprint,
            "endpoint_fingerprint",
        ),
        (lease.model_id == lease.attestation.model_id, "model_id"),
        (
            lease.model_tier == lease.attestation.model_tier,
            "model_tier",
        ),
    ] {
        if !matches {
            return Err(EnterpriseReviewAdmissionError::AttestationMismatch(name));
        }
    }
    if !lease.attestation.no_premium_fallback {
        return Err(EnterpriseReviewAdmissionError::FallbackPermitted);
    }
    if !lease.attestation.egress_firewall_attested {
        return Err(EnterpriseReviewAdmissionError::EgressFirewallMissing);
    }
    if !lease.read_only || !policy.read_only {
        return Err(EnterpriseReviewAdmissionError::ReviewMustBeReadOnly);
    }
    if lease.allow_network || policy.allow_network {
        return Err(EnterpriseReviewAdmissionError::NetworkNotAllowed);
    }
    if lease.allow_workspace_writes || policy.allow_workspace_writes {
        return Err(EnterpriseReviewAdmissionError::WorkspaceWritesNotAllowed);
    }
    if lease.allow_publication || policy.allow_publication {
        return Err(EnterpriseReviewAdmissionError::PublicationNotAllowed);
    }
    for (requested, granted, maximum, name) in [
        (
            policy.max_requests as u64,
            lease.max_requests as u64,
            MAX_ENTERPRISE_REVIEW_REQUESTS as u64,
            "requests",
        ),
        (
            policy.max_tokens,
            lease.max_tokens,
            MAX_ENTERPRISE_REVIEW_TOKENS,
            "tokens",
        ),
        (
            policy.max_duration_ms,
            lease.max_duration_ms,
            MAX_ENTERPRISE_REVIEW_DURATION_MS,
            "duration_ms",
        ),
    ] {
        if requested == 0 || requested > granted || requested > maximum {
            return Err(EnterpriseReviewAdmissionError::BoundExceeded(name));
        }
    }
    if expected_route_binding_digest(lease) != lease.route_binding_digest {
        return Err(EnterpriseReviewAdmissionError::RouteBindingMismatch);
    }
    Ok(EnterpriseReviewEvidence {
        schema: ENTERPRISE_REVIEW_EVIDENCE_SCHEMA.to_owned(),
        lease_id: lease.lease_id.clone(),
        route_id: lease.route_id.clone(),
        model_id: lease.model_id.clone(),
        model_tier: lease.model_tier,
        route_binding_digest: lease.route_binding_digest.clone(),
        policy_digest: policy_digest(policy),
        read_only: true,
        no_premium_fallback: true,
        egress_firewall_attested: true,
        secret_free: true,
    })
}

pub fn expected_route_binding_digest(lease: &EnterpriseReviewLease) -> String {
    let mut hasher = Sha256::new();
    hasher.update(lease.route_id.as_bytes());
    hasher.update([0]);
    hasher.update(lease.endpoint_fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(lease.model_id.as_bytes());
    hasher.update([0]);
    hasher.update(lease.credential_id.as_bytes());
    hex_digest(hasher.finalize())
}

fn policy_digest(policy: &EnterpriseReviewPolicy) -> String {
    let bytes = serde_json::to_vec(policy).expect("policy serialization is infallible");
    hex_digest(Sha256::digest(bytes))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(now: DateTime<Utc>) -> EnterpriseReviewLease {
        let mut lease = EnterpriseReviewLease {
            schema: ENTERPRISE_REVIEW_LEASE_SCHEMA.into(),
            lease_id: "lease-1".into(),
            credential_id: "credential-opaque".into(),
            route_id: "company-gateway".into(),
            endpoint_fingerprint: "a".repeat(64),
            model_id: "modest-review-v1".into(),
            model_tier: EnterpriseModelTier::Modest,
            issued_at: now - chrono::Duration::minutes(1),
            expires_at: now + chrono::Duration::hours(1),
            route_binding_digest: String::new(),
            read_only: true,
            allow_network: false,
            allow_workspace_writes: false,
            allow_publication: false,
            max_requests: MAX_ENTERPRISE_REVIEW_REQUESTS,
            max_tokens: MAX_ENTERPRISE_REVIEW_TOKENS,
            max_duration_ms: MAX_ENTERPRISE_REVIEW_DURATION_MS,
            attestation: EnterpriseGatewayAttestation {
                schema: ENTERPRISE_REVIEW_ATTESTATION_SCHEMA.into(),
                route_id: "company-gateway".into(),
                endpoint_fingerprint: "a".repeat(64),
                model_id: "modest-review-v1".into(),
                model_tier: EnterpriseModelTier::Modest,
                deployment_revision: "deploy-1".into(),
                issued_at: now - chrono::Duration::minutes(1),
                expires_at: now + chrono::Duration::hours(1),
                no_premium_fallback: true,
                egress_firewall_attested: true,
            },
        };
        lease.route_binding_digest = expected_route_binding_digest(&lease);
        lease
    }

    #[test]
    fn valid_lease_returns_secret_free_evidence() {
        let now = Utc::now();
        let lease = lease(now);
        let policy = EnterpriseReviewPolicy::default();
        let evidence = admit_enterprise_review(&lease, &policy, now).unwrap();
        assert!(evidence.secret_free);
        evidence.validate().unwrap();
        let encoded = serde_json::to_string(&evidence).unwrap();
        assert!(!encoded.contains("credential-opaque"));
        assert!(!encoded.contains("https://"));
    }

    #[test]
    fn evidence_projection_rejects_broadened_policy_or_secrets() {
        let now = Utc::now();
        let lease = lease(now);
        let policy = EnterpriseReviewPolicy::default();
        let mut evidence = admit_enterprise_review(&lease, &policy, now).unwrap();
        evidence.read_only = false;
        assert_eq!(
            evidence.validate(),
            Err(EnterpriseReviewAdmissionError::ReviewMustBeReadOnly)
        );
        evidence.read_only = true;
        evidence.secret_free = false;
        assert_eq!(
            evidence.validate(),
            Err(EnterpriseReviewAdmissionError::InvalidField("secret_free"))
        );
    }

    #[test]
    fn expiry_fallback_and_route_drift_fail_closed() {
        let now = Utc::now();
        let mut expired = lease(now);
        expired.expires_at = now;
        assert_eq!(
            admit_enterprise_review(&expired, &EnterpriseReviewPolicy::default(), now),
            Err(EnterpriseReviewAdmissionError::Expired)
        );

        let mut fallback = lease(now);
        fallback.attestation.no_premium_fallback = false;
        assert_eq!(
            admit_enterprise_review(&fallback, &EnterpriseReviewPolicy::default(), now),
            Err(EnterpriseReviewAdmissionError::FallbackPermitted)
        );

        let mut drift = lease(now);
        drift.model_id = "different-model".into();
        assert_eq!(
            admit_enterprise_review(&drift, &EnterpriseReviewPolicy::default(), now),
            Err(EnterpriseReviewAdmissionError::AttestationMismatch(
                "model_id"
            ))
        );
    }

    #[test]
    fn write_network_publish_and_bound_requests_are_denied() {
        let now = Utc::now();
        let base = lease(now);
        for mutation in [
            |lease: &mut EnterpriseReviewLease| lease.read_only = false,
            |lease: &mut EnterpriseReviewLease| lease.allow_network = true,
            |lease: &mut EnterpriseReviewLease| lease.allow_workspace_writes = true,
            |lease: &mut EnterpriseReviewLease| lease.allow_publication = true,
        ] {
            let mut mutated = base.clone();
            mutation(&mut mutated);
            mutated.route_binding_digest = expected_route_binding_digest(&mutated);
            assert!(
                admit_enterprise_review(&mutated, &EnterpriseReviewPolicy::default(), now).is_err()
            );
        }
        let policy = EnterpriseReviewPolicy {
            max_requests: MAX_ENTERPRISE_REVIEW_REQUESTS + 1,
            ..EnterpriseReviewPolicy::default()
        };
        assert_eq!(
            admit_enterprise_review(&base, &policy, now),
            Err(EnterpriseReviewAdmissionError::BoundExceeded("requests"))
        );
    }

    #[test]
    fn schema_rejects_unknown_fields() {
        let now = Utc::now();
        let mut value = serde_json::to_value(lease(now)).unwrap();
        value["unexpected"] = serde_json::json!(true);
        let error = serde_json::from_value::<EnterpriseReviewLease>(value).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
