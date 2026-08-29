//! Live-provider continuation. Disabled by default. Same schemas; fake PASS
//! does not satisfy live eligibility.

use serde::{Deserialize, Serialize};

use crate::digest::sha256_hex;
use crate::schema::to_canonical_json;
use crate::types::{Eligibility, EvalError, EvalResult};

pub const LIVE_ENV: &str = "GROKPTAH_CU_ADAPTIVE_LIVE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct BillingReceipt {
    pub currency: String,
    pub amount_micros: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ProviderReceipt {
    pub receipt_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub config_digest: String,
    pub usage_digest: Option<String>,
    pub content_sha256: String,
    pub billing: Option<BillingReceipt>,
}

impl ProviderReceipt {
    pub fn compute_content_sha256(&self) -> EvalResult<String> {
        let mut clone = self.clone();
        clone.content_sha256 = String::new();
        let json = to_canonical_json(&clone)?;
        Ok(sha256_hex(json.as_bytes()))
    }

    pub fn validate(&self) -> EvalResult<()> {
        crate::types::validate_id("receipt_id", &self.receipt_id)?;
        crate::types::validate_id("provider_id", &self.provider_id)?;
        if self.model_id.is_empty() || self.config_digest.len() != 64 {
            return Err(EvalError::Schema(
                "provider receipt identity is incomplete".into(),
            ));
        }
        let expected = self.compute_content_sha256()?;
        if expected != self.content_sha256 {
            return Err(EvalError::Verifier(
                "provider receipt content digest mismatch".into(),
            ));
        }
        if let Some(billing) = &self.billing {
            if billing.digest.len() != 64 || billing.currency.is_empty() {
                return Err(EvalError::Schema("billing receipt is incomplete".into()));
            }
        }
        Ok(())
    }
}

pub fn live_requested() -> bool {
    matches!(std::env::var(LIVE_ENV), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Live eligibility requires a structured receipt, never a caller Boolean.
pub fn live_eligibility(provider_calls: u64, receipt: Option<&ProviderReceipt>) -> Eligibility {
    if provider_calls == 0 || receipt.is_none() {
        Eligibility::SyntheticOnly
    } else if receipt.map(|r| r.validate().is_ok()).unwrap_or(false) {
        Eligibility::LiveAuthoritative
    } else {
        Eligibility::LiveReusableSchema
    }
}

pub fn refuse_if_not_explicitly_enabled() -> EvalResult<()> {
    if live_requested() {
        Err(EvalError::Host(
            "live provider continuation is wired to the same schemas but is not implemented in this evaluation lane; refuse rather than call a provider".into(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_cannot_grant_live_authority() {
        assert_eq!(live_eligibility(0, None), Eligibility::SyntheticOnly);
        assert_eq!(live_eligibility(1, None), Eligibility::SyntheticOnly);
    }
}
