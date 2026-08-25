//! Secret-free binding for a live Grok Build campaign report.
//!
//! The attestation and quota receipt contracts are useful independently, but
//! the Stage 2 exit needs one artifact proving they describe the same named
//! campaign, credential binding, and route/model. This module provides that
//! durable projection. It never carries tokens, client identifiers, URLs, or
//! provider response bodies.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::live_attestation::LiveCredentialAttestation;
use crate::provider_quota_receipt::{ProviderQuotaReceiptError, ProviderQuotaReceiptSet};

pub const LIVE_PROVIDER_CAMPAIGN_EVIDENCE_SCHEMA: &str =
    "grokptah.live-provider-campaign-evidence.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LiveProviderCampaignEvidence {
    pub schema: String,
    pub campaign_id: String,
    pub attestation_binding_id: String,
    pub credential_fingerprint: String,
    pub route_binding_digest: String,
    pub attestation_ready: bool,
    pub quota_receipt_set: ProviderQuotaReceiptSet,
    pub secret_free: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveProviderCampaignEvidenceError {
    InvalidField(&'static str),
    UnsupportedSchema,
    AttestationNotReady,
    BindingMismatch(&'static str),
    QuotaReceipt(ProviderQuotaReceiptError),
}

impl std::fmt::Display for LiveProviderCampaignEvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(name) => write!(f, "invalid live campaign evidence field: {name}"),
            Self::UnsupportedSchema => write!(f, "unsupported live campaign evidence schema"),
            Self::AttestationNotReady => write!(f, "live credential attestation is not ready"),
            Self::BindingMismatch(name) => write!(f, "live campaign binding mismatch: {name}"),
            Self::QuotaReceipt(error) => write!(f, "invalid live quota receipt: {error}"),
        }
    }
}

impl std::error::Error for LiveProviderCampaignEvidenceError {}

impl From<ProviderQuotaReceiptError> for LiveProviderCampaignEvidenceError {
    fn from(error: ProviderQuotaReceiptError) -> Self {
        Self::QuotaReceipt(error)
    }
}

impl LiveProviderCampaignEvidence {
    /// Assemble a campaign projection only from a positive live attestation
    /// and a complete, bound consumption-plus-429 receipt set.
    pub fn from_attestation(
        campaign_id: impl Into<String>,
        attestation: &LiveCredentialAttestation,
        quota_receipt_set: ProviderQuotaReceiptSet,
    ) -> Result<Self, LiveProviderCampaignEvidenceError> {
        if !attestation.certification_ready() {
            return Err(LiveProviderCampaignEvidenceError::AttestationNotReady);
        }
        quota_receipt_set.validate()?;
        let mut evidence = Self {
            schema: LIVE_PROVIDER_CAMPAIGN_EVIDENCE_SCHEMA.to_owned(),
            campaign_id: campaign_id.into(),
            attestation_binding_id: attestation.binding_id().as_str().to_owned(),
            credential_fingerprint: attestation.credential_fingerprint(),
            route_binding_digest: attestation.route_binding_digest(),
            attestation_ready: true,
            quota_receipt_set,
            secret_free: true,
            evidence_digest: String::new(),
        };
        evidence.evidence_digest = expected_evidence_digest(&evidence);
        evidence.validate()?;
        Ok(evidence)
    }

    pub fn validate(&self) -> Result<(), LiveProviderCampaignEvidenceError> {
        if self.schema != LIVE_PROVIDER_CAMPAIGN_EVIDENCE_SCHEMA {
            return Err(LiveProviderCampaignEvidenceError::UnsupportedSchema);
        }
        if !valid_opaque_id(&self.campaign_id)
            || !valid_attestation_binding_id(&self.attestation_binding_id)
            || !valid_fingerprint(&self.credential_fingerprint)
            || !valid_fingerprint(&self.route_binding_digest)
            || !valid_fingerprint(&self.evidence_digest)
        {
            return Err(LiveProviderCampaignEvidenceError::InvalidField("binding"));
        }
        if !self.attestation_ready {
            return Err(LiveProviderCampaignEvidenceError::AttestationNotReady);
        }
        if !self.secret_free {
            return Err(LiveProviderCampaignEvidenceError::InvalidField(
                "secret_free",
            ));
        }
        self.quota_receipt_set.validate()?;
        if self.quota_receipt_set.campaign_id != self.campaign_id {
            return Err(LiveProviderCampaignEvidenceError::BindingMismatch(
                "campaign_id",
            ));
        }
        if self.quota_receipt_set.credential_fingerprint != self.credential_fingerprint {
            return Err(LiveProviderCampaignEvidenceError::BindingMismatch(
                "credential_fingerprint",
            ));
        }
        if self.quota_receipt_set.route_binding_digest != self.route_binding_digest {
            return Err(LiveProviderCampaignEvidenceError::BindingMismatch(
                "route_binding_digest",
            ));
        }
        if self.evidence_digest != expected_evidence_digest(self) {
            return Err(LiveProviderCampaignEvidenceError::InvalidField(
                "evidence_digest",
            ));
        }
        Ok(())
    }

    pub fn certification_ready(&self) -> bool {
        self.validate().is_ok()
    }
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

fn valid_attestation_binding_id(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("opaque-")
        && value[7..]
            .bytes()
            .all(|byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn expected_evidence_digest(evidence: &LiveProviderCampaignEvidence) -> String {
    let mut unsigned = evidence.clone();
    unsigned.evidence_digest.clear();
    let encoded = serde_json::to_vec(&unsigned).expect("live evidence serialization is infallible");
    format!("{:x}", Sha256::digest(encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_quota_receipt::{
        expected_receipt_digest, ProviderQuotaReceipt, ProviderQuotaReceiptKind,
        PROVIDER_QUOTA_RECEIPT_SCHEMA, PROVIDER_QUOTA_RECEIPT_SET_SCHEMA,
    };
    use chrono::{Duration, Utc};

    fn quota(campaign_id: &str, credential: &str, route: &str) -> ProviderQuotaReceiptSet {
        let now = Utc::now();
        let mut consumed = ProviderQuotaReceipt {
            schema: PROVIDER_QUOTA_RECEIPT_SCHEMA.into(),
            campaign_id: campaign_id.into(),
            credential_fingerprint: credential.into(),
            route_binding_digest: route.into(),
            kind: ProviderQuotaReceiptKind::Consumed,
            request_id_digest: "c".repeat(64),
            observed_at: now,
            request_count: 1,
            token_count: 1,
            status_code: None,
            receipt_digest: String::new(),
            secret_free: true,
        };
        consumed.receipt_digest = expected_receipt_digest(&consumed);
        let mut exhausted = ProviderQuotaReceipt {
            schema: PROVIDER_QUOTA_RECEIPT_SCHEMA.into(),
            campaign_id: campaign_id.into(),
            credential_fingerprint: credential.into(),
            route_binding_digest: route.into(),
            kind: ProviderQuotaReceiptKind::Exhausted,
            request_id_digest: "d".repeat(64),
            observed_at: now + Duration::seconds(1),
            request_count: 0,
            token_count: 0,
            status_code: Some(429),
            receipt_digest: String::new(),
            secret_free: true,
        };
        exhausted.receipt_digest = expected_receipt_digest(&exhausted);
        ProviderQuotaReceiptSet {
            schema: PROVIDER_QUOTA_RECEIPT_SET_SCHEMA.into(),
            campaign_id: campaign_id.into(),
            credential_fingerprint: credential.into(),
            route_binding_digest: route.into(),
            consumed,
            exhausted,
            secret_free: true,
        }
    }

    fn evidence() -> LiveProviderCampaignEvidence {
        let credential = "a".repeat(64);
        let route = "b".repeat(64);
        LiveProviderCampaignEvidence {
            schema: LIVE_PROVIDER_CAMPAIGN_EVIDENCE_SCHEMA.into(),
            campaign_id: "campaign-live".into(),
            attestation_binding_id: "opaque-".to_owned() + &"c".repeat(64),
            credential_fingerprint: credential.clone(),
            route_binding_digest: route.clone(),
            attestation_ready: true,
            quota_receipt_set: quota("campaign-live", &credential, &route),
            secret_free: true,
            evidence_digest: String::new(),
        }
    }

    #[test]
    fn bound_live_campaign_evidence_is_ready_and_secret_free() {
        let evidence = evidence();
        let mut evidence = evidence;
        evidence.evidence_digest = expected_evidence_digest(&evidence);
        evidence.validate().unwrap();
        assert!(evidence.certification_ready());
        let encoded = serde_json::to_string(&evidence).unwrap();
        assert!(!encoded.contains("bearer"));
        assert!(!encoded.contains("https://"));
    }

    #[test]
    fn campaign_binding_drift_and_unready_attestation_fail_closed() {
        let mut drift = evidence();
        drift.evidence_digest = expected_evidence_digest(&drift);
        drift.campaign_id = "other-campaign".into();
        assert_eq!(
            drift.validate(),
            Err(LiveProviderCampaignEvidenceError::BindingMismatch(
                "campaign_id"
            ))
        );
        let mut unready = evidence();
        unready.evidence_digest = expected_evidence_digest(&unready);
        unready.attestation_ready = false;
        assert_eq!(
            unready.validate(),
            Err(LiveProviderCampaignEvidenceError::AttestationNotReady)
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut value = serde_json::to_value(evidence()).unwrap();
        value["evidence_digest"] = serde_json::json!("a".repeat(64));
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<LiveProviderCampaignEvidence>(value).is_err());
    }

    #[test]
    fn evidence_digest_detects_transport_tampering() {
        let mut evidence = evidence();
        evidence.evidence_digest = expected_evidence_digest(&evidence);
        evidence.attestation_binding_id = "opaque-".to_owned() + &"e".repeat(64);
        assert_eq!(
            evidence.validate(),
            Err(LiveProviderCampaignEvidenceError::InvalidField(
                "evidence_digest"
            ))
        );
    }
}
