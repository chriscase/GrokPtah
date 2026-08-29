//! Profile naming decision packet.
//!
//! Issue #435 is authoritative: Economy / Balanced / High Assurance.
//! Unmerged runtime candidates on developer checkouts expose Efficient /
//! Balanced / Frontier. This crate does not treat those as product names and
//! does not silently change product semantics.

use crate::types::{EvalError, EvalResult, ProfileId};

pub const CANONICAL_NAMES: [&str; 3] = ["economy", "balanced", "high_assurance"];

pub const DECISION: &str = "Canonical evaluation and report identifiers are issue #435 names \
economy, balanced, and high_assurance. Compatibility aliases efficient→economy and \
frontier→high_assurance may be accepted on ingest of unmerged-runtime evidence, then \
canonicalized. Aliases are not distinct profiles, not a safety-mode ladder, and not a \
rename of the product contract. Economy is an efficiency policy (less observation, \
action, and model cost) with identical safety authority. This evaluation lane does not \
edit production adaptive-profile identifiers.";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct NamingRecord {
    pub canonical: Vec<String>,
    pub aliases: NamingAliases,
    pub decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamingAliases {
    pub efficient: String,
    pub frontier: String,
}

impl NamingRecord {
    pub fn decision_packet() -> Self {
        Self {
            canonical: CANONICAL_NAMES.iter().map(|s| (*s).to_string()).collect(),
            aliases: NamingAliases {
                efficient: "economy".into(),
                frontier: "high_assurance".into(),
            },
            decision: DECISION.into(),
        }
    }
}

/// Parse a profile token. Aliases are accepted only as compatibility ingest.
pub fn parse_profile(raw: &str) -> EvalResult<ProfileId> {
    match raw {
        "economy" => Ok(ProfileId::Economy),
        "balanced" => Ok(ProfileId::Balanced),
        "high_assurance" => Ok(ProfileId::HighAssurance),
        "efficient" => Ok(ProfileId::Economy),
        "frontier" => Ok(ProfileId::HighAssurance),
        other => Err(EvalError::Schema(format!("unknown profile {other}"))),
    }
}

/// Reports always emit canonical #435 names, never aliases.
pub fn canonical_name(id: ProfileId) -> &'static str {
    id.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_do_not_invent_a_fourth_profile() {
        assert_eq!(parse_profile("efficient").unwrap(), ProfileId::Economy);
        assert_eq!(parse_profile("frontier").unwrap(), ProfileId::HighAssurance);
        assert_eq!(
            canonical_name(parse_profile("efficient").unwrap()),
            "economy"
        );
        assert_eq!(
            canonical_name(parse_profile("frontier").unwrap()),
            "high_assurance"
        );
        assert_eq!(ProfileId::ALL.len(), 3);
    }
}
