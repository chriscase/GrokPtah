//! Live-provider continuation. Disabled by default. Same schemas; fake PASS
//! does not satisfy live eligibility.

use crate::types::{Eligibility, EvalError, EvalResult};

pub const LIVE_ENV: &str = "GROKPTAH_CU_ADAPTIVE_LIVE";

pub fn live_requested() -> bool {
    matches!(std::env::var(LIVE_ENV), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Live runs must reuse the v1 scenario/result/evidence/report schemas and
/// cannot inherit synthetic_only eligibility.
pub fn live_eligibility(provider_calls: u64, authoritative_receipt: bool) -> Eligibility {
    if provider_calls == 0 {
        Eligibility::SyntheticOnly
    } else if authoritative_receipt {
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
    fn fake_results_are_not_live_authoritative() {
        assert_eq!(live_eligibility(0, true), Eligibility::SyntheticOnly);
        assert_eq!(live_eligibility(1, false), Eligibility::LiveReusableSchema);
        assert_eq!(live_eligibility(1, true), Eligibility::LiveAuthoritative);
    }
}
