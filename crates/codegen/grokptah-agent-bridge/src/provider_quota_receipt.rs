//! Secret-free provider-quota evidence for live certification campaigns.
//!
//! A local token counter or a hermetic fake `429` is not proof that a named
//! provider account consumed and then exhausted quota.  This contract lets an
//! operator-owned campaign attach two independently observed, route-bound
//! receipts—consumption and exhaustion—without placing a bearer, URL, raw
//! response, or account balance in the product evidence.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PROVIDER_QUOTA_RECEIPT_SCHEMA: &str = "grokptah.provider-quota-receipt.v1";
pub const PROVIDER_QUOTA_RECEIPT_SET_SCHEMA: &str = "grokptah.provider-quota-receipt-set.v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderQuotaReceiptKind {
    Consumed,
    Exhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderQuotaReceipt {
    pub schema: String,
    pub campaign_id: String,
    pub credential_fingerprint: String,
    pub route_binding_digest: String,
    pub kind: ProviderQuotaReceiptKind,
    pub request_id_digest: String,
    pub observed_at: DateTime<Utc>,
    pub request_count: u32,
    pub token_count: u64,
    pub status_code: Option<u16>,
    pub receipt_digest: String,
    pub secret_free: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderQuotaReceiptSet {
    pub schema: String,
    pub campaign_id: String,
    pub credential_fingerprint: String,
    pub route_binding_digest: String,
    pub consumed: ProviderQuotaReceipt,
    pub exhausted: ProviderQuotaReceipt,
    pub secret_free: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderQuotaReceiptError {
    InvalidField(&'static str),
    UnsupportedSchema,
    BindingMismatch(&'static str),
    WrongKind,
    ExhaustionNot429,
    NotOrdered,
    DuplicateObservation,
}

impl std::fmt::Display for ProviderQuotaReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField(name) => write!(f, "invalid provider quota receipt field: {name}"),
            Self::UnsupportedSchema => write!(f, "unsupported provider quota receipt schema"),
            Self::BindingMismatch(name) => {
                write!(f, "provider quota receipt binding mismatch: {name}")
            }
            Self::WrongKind => write!(f, "provider quota receipt has the wrong event kind"),
            Self::ExhaustionNot429 => {
                write!(f, "provider quota exhaustion must be an observed HTTP 429")
            }
            Self::NotOrdered => write!(f, "provider quota exhaustion precedes consumption"),
            Self::DuplicateObservation => write!(f, "provider quota observations are not distinct"),
        }
    }
}

impl std::error::Error for ProviderQuotaReceiptError {}

impl ProviderQuotaReceipt {
    pub fn validate(&self) -> Result<(), ProviderQuotaReceiptError> {
        if self.schema != PROVIDER_QUOTA_RECEIPT_SCHEMA {
            return Err(ProviderQuotaReceiptError::UnsupportedSchema);
        }
        if !valid_opaque_id(&self.campaign_id) {
            return Err(ProviderQuotaReceiptError::InvalidField("campaign_id"));
        }
        for (value, name) in [
            (&self.credential_fingerprint, "credential_fingerprint"),
            (&self.route_binding_digest, "route_binding_digest"),
            (&self.request_id_digest, "request_id_digest"),
            (&self.receipt_digest, "receipt_digest"),
        ] {
            if !valid_fingerprint(value) {
                return Err(ProviderQuotaReceiptError::InvalidField(name));
            }
        }
        if !self.secret_free || self.observed_at.timestamp() < 0 {
            return Err(ProviderQuotaReceiptError::InvalidField("secret_free"));
        }
        match self.kind {
            ProviderQuotaReceiptKind::Consumed => {
                if self.request_count == 0 || self.token_count == 0 || self.status_code.is_some() {
                    return Err(ProviderQuotaReceiptError::InvalidField("consumed"));
                }
            }
            ProviderQuotaReceiptKind::Exhausted => {
                if self.status_code != Some(429) || self.request_count != 0 || self.token_count != 0
                {
                    return Err(ProviderQuotaReceiptError::ExhaustionNot429);
                }
            }
        }
        if self.receipt_digest != expected_receipt_digest(self) {
            return Err(ProviderQuotaReceiptError::InvalidField("receipt_digest"));
        }
        Ok(())
    }
}

impl ProviderQuotaReceiptSet {
    pub fn validate(&self) -> Result<(), ProviderQuotaReceiptError> {
        if self.schema != PROVIDER_QUOTA_RECEIPT_SET_SCHEMA || !self.secret_free {
            return Err(ProviderQuotaReceiptError::UnsupportedSchema);
        }
        self.consumed.validate()?;
        self.exhausted.validate()?;
        for (receipt, name) in [(&self.consumed, "consumed"), (&self.exhausted, "exhausted")] {
            if receipt.campaign_id != self.campaign_id {
                return Err(ProviderQuotaReceiptError::BindingMismatch(name));
            }
            if receipt.credential_fingerprint != self.credential_fingerprint {
                return Err(ProviderQuotaReceiptError::BindingMismatch(
                    "credential_fingerprint",
                ));
            }
            if receipt.route_binding_digest != self.route_binding_digest {
                return Err(ProviderQuotaReceiptError::BindingMismatch(
                    "route_binding_digest",
                ));
            }
        }
        if self.consumed.kind != ProviderQuotaReceiptKind::Consumed
            || self.exhausted.kind != ProviderQuotaReceiptKind::Exhausted
        {
            return Err(ProviderQuotaReceiptError::WrongKind);
        }
        if self.consumed.request_id_digest == self.exhausted.request_id_digest
            || self.consumed.receipt_digest == self.exhausted.receipt_digest
        {
            return Err(ProviderQuotaReceiptError::DuplicateObservation);
        }
        if self.exhausted.observed_at < self.consumed.observed_at {
            return Err(ProviderQuotaReceiptError::NotOrdered);
        }
        Ok(())
    }

    /// True only for a complete, bound consumption-plus-exhaustion pair. This
    /// remains evidence for a named live campaign, not account-balance sync.
    pub fn certification_ready(&self) -> bool {
        self.validate().is_ok()
    }
}

pub fn expected_receipt_digest(receipt: &ProviderQuotaReceipt) -> String {
    let mut hasher = Sha256::new();
    for value in [
        receipt.schema.as_str(),
        receipt.campaign_id.as_str(),
        receipt.credential_fingerprint.as_str(),
        receipt.route_binding_digest.as_str(),
        match receipt.kind {
            ProviderQuotaReceiptKind::Consumed => "consumed",
            ProviderQuotaReceiptKind::Exhausted => "exhausted",
        },
        receipt.request_id_digest.as_str(),
        &receipt.observed_at.to_rfc3339(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    hasher.update(receipt.request_count.to_be_bytes());
    hasher.update(receipt.token_count.to_be_bytes());
    hasher.update(receipt.status_code.unwrap_or_default().to_be_bytes());
    hasher
        .finalize()
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
    use chrono::Duration;

    fn receipt(
        kind: ProviderQuotaReceiptKind,
        observed_at: DateTime<Utc>,
        request_id: &str,
    ) -> ProviderQuotaReceipt {
        let mut receipt = ProviderQuotaReceipt {
            schema: PROVIDER_QUOTA_RECEIPT_SCHEMA.into(),
            campaign_id: "campaign-quota".into(),
            credential_fingerprint: "a".repeat(64),
            route_binding_digest: "b".repeat(64),
            kind,
            request_id_digest: request_id.into(),
            observed_at,
            request_count: if kind == ProviderQuotaReceiptKind::Consumed {
                3
            } else {
                0
            },
            token_count: if kind == ProviderQuotaReceiptKind::Consumed {
                120
            } else {
                0
            },
            status_code: (kind == ProviderQuotaReceiptKind::Exhausted).then_some(429),
            receipt_digest: String::new(),
            secret_free: true,
        };
        receipt.receipt_digest = expected_receipt_digest(&receipt);
        receipt
    }

    fn set() -> ProviderQuotaReceiptSet {
        let now = Utc::now();
        ProviderQuotaReceiptSet {
            schema: PROVIDER_QUOTA_RECEIPT_SET_SCHEMA.into(),
            campaign_id: "campaign-quota".into(),
            credential_fingerprint: "a".repeat(64),
            route_binding_digest: "b".repeat(64),
            consumed: receipt(ProviderQuotaReceiptKind::Consumed, now, &"c".repeat(64)),
            exhausted: receipt(
                ProviderQuotaReceiptKind::Exhausted,
                now + Duration::seconds(1),
                &"d".repeat(64),
            ),
            secret_free: true,
        }
    }

    #[test]
    fn complete_bound_pair_is_certification_ready_and_secret_free() {
        let pair = set();
        pair.validate().unwrap();
        assert!(pair.certification_ready());
        let encoded = serde_json::to_string(&pair).unwrap();
        assert!(!encoded.contains("https://"));
        assert!(!encoded.contains("api_key"));
        assert!(!encoded.contains("bearer"));
    }

    #[test]
    fn route_or_credential_drift_and_order_fail_closed() {
        let mut pair = set();
        pair.exhausted.route_binding_digest = "e".repeat(64);
        pair.exhausted.receipt_digest = expected_receipt_digest(&pair.exhausted);
        assert!(matches!(
            pair.validate(),
            Err(ProviderQuotaReceiptError::BindingMismatch(
                "route_binding_digest"
            ))
        ));

        let mut reversed = set();
        reversed.exhausted.observed_at = reversed.consumed.observed_at - Duration::seconds(1);
        reversed.exhausted.receipt_digest = expected_receipt_digest(&reversed.exhausted);
        assert_eq!(
            reversed.validate(),
            Err(ProviderQuotaReceiptError::NotOrdered)
        );
    }

    #[test]
    fn fake_429_or_consumption_without_pair_is_not_ready() {
        let mut pair = set();
        pair.exhausted.status_code = Some(500);
        pair.exhausted.receipt_digest = expected_receipt_digest(&pair.exhausted);
        assert_eq!(
            pair.validate(),
            Err(ProviderQuotaReceiptError::ExhaustionNot429)
        );

        let mut nonzero_exhaustion = set();
        nonzero_exhaustion.exhausted.token_count = 1;
        nonzero_exhaustion.exhausted.receipt_digest =
            expected_receipt_digest(&nonzero_exhaustion.exhausted);
        assert_eq!(
            nonzero_exhaustion.validate(),
            Err(ProviderQuotaReceiptError::ExhaustionNot429)
        );

        let mut incomplete = set();
        incomplete.exhausted.request_id_digest = incomplete.consumed.request_id_digest.clone();
        incomplete.exhausted.receipt_digest = expected_receipt_digest(&incomplete.exhausted);
        assert_eq!(
            incomplete.validate(),
            Err(ProviderQuotaReceiptError::DuplicateObservation)
        );
    }

    #[test]
    fn tampered_receipt_digest_and_unknown_fields_fail_closed() {
        let mut tampered = set().consumed;
        tampered.token_count += 1;
        assert_eq!(
            tampered.validate(),
            Err(ProviderQuotaReceiptError::InvalidField("receipt_digest"))
        );
        let mut value = serde_json::to_value(receipt(
            ProviderQuotaReceiptKind::Consumed,
            Utc::now(),
            &"f".repeat(64),
        ))
        .unwrap();
        value["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ProviderQuotaReceipt>(value).is_err());
    }
}
