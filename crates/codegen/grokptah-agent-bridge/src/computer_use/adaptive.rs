//! Canonical adaptive execution vocabulary and fail-closed packaged claims.
//!
//! Issue #435 names exactly three execution profiles: **Economy**,
//! **Balanced**, and **High Assurance**. Several unmerged candidates spell the
//! same three ideas `Efficient` / `Balanced` / `Frontier`. Carrying two
//! spellings in one repository is how a rename silently becomes a second
//! policy, so this module fixes one canonical vocabulary and admits the older
//! spellings as **ingest-only aliases**: they deserialize, they never
//! serialize, and they resolve to byte-identical budgets.
//!
//! A profile is an **efficiency** policy. It bounds how much observation,
//! image, and model budget a run may spend. It never bounds a safety check:
//! [`AdaptiveBudget`] carries no field that can relax staleness, sensitivity,
//! grant, target, or element authority, and [`super::policy::ComputerPolicy`]
//! never reads a profile. Economy is not a weaker-safety mode.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{ComputerError, ComputerErrorCode, ComputerResult};

/// The canonical execution profile vocabulary.
///
/// Serialization is always canonical (`economy` / `balanced` /
/// `high_assurance`). `efficient` and `frontier` are accepted on the way in
/// so a record or config written against an older candidate still loads, and
/// are never produced on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveProfile {
    /// Semantic-first. No image budget at all, tight turn budget, high
    /// abstention. Same safety checks as every other profile.
    #[serde(alias = "efficient")]
    Economy,
    /// Semantic-first with a bounded visual-grounding allowance.
    Balanced,
    /// Strongest eligible path with the richest verification allowance.
    #[serde(alias = "frontier")]
    HighAssurance,
}

impl AdaptiveProfile {
    /// The canonical wire identifier. Never an alias.
    pub fn canonical_str(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::HighAssurance => "high_assurance",
        }
    }

    /// Resolve a canonical name or an ingest-only alias.
    ///
    /// Unknown input fails closed rather than defaulting to a profile: an
    /// unrecognized profile name is a configuration error, and guessing one
    /// would be guessing a budget.
    pub fn ingest(value: &str) -> ComputerResult<Self> {
        match value {
            "economy" | "efficient" => Ok(Self::Economy),
            "balanced" => Ok(Self::Balanced),
            "high_assurance" | "frontier" => Ok(Self::HighAssurance),
            _ => Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "unknown adaptive profile",
            )),
        }
    }

    /// True when `value` is an accepted spelling that is not the canonical one.
    pub fn is_ingest_alias(value: &str) -> bool {
        matches!(value, "efficient" | "frontier")
    }

    /// The cost envelope for this profile.
    pub fn budget(self) -> AdaptiveBudget {
        match self {
            Self::Economy => AdaptiveBudget {
                max_model_turns: 8,
                max_image_bytes: 0,
                max_repairs: 1,
                max_turn_millis: 20_000,
            },
            Self::Balanced => AdaptiveBudget {
                max_model_turns: 16,
                max_image_bytes: 2 * 1024 * 1024,
                max_repairs: 2,
                max_turn_millis: 45_000,
            },
            Self::HighAssurance => AdaptiveBudget {
                max_model_turns: 32,
                max_image_bytes: 8 * 1024 * 1024,
                max_repairs: 3,
                max_turn_millis: 90_000,
            },
        }
    }
}

/// The cost envelope of a profile.
///
/// Every field bounds spend. None of them bounds a safety check — that is the
/// invariant the vocabulary exists to protect, and
/// `budgets_never_encode_a_safety_relaxation` pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveBudget {
    pub max_model_turns: u32,
    /// Economy is deliberately `0`: a semantic-only profile sends no pixels.
    pub max_image_bytes: u64,
    pub max_repairs: u32,
    pub max_turn_millis: u64,
}

impl AdaptiveBudget {
    /// True when `self` permits no more than `other` on every axis.
    pub fn is_within(self, other: Self) -> bool {
        self.max_model_turns <= other.max_model_turns
            && self.max_image_bytes <= other.max_image_bytes
            && self.max_repairs <= other.max_repairs
            && self.max_turn_millis <= other.max_turn_millis
    }
}

/// Honest provider accounting.
///
/// `attempts` is incremented **before** a request leaves, so a timeout, a
/// transport failure, prose, or a schema refusal costs exactly what a success
/// costs. `attempts` always equals `accepted + rejected`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveSpend {
    pub attempts: u32,
    pub accepted: u32,
    pub rejected: u32,
    pub image_bytes: u64,
    /// Provider-reported token usage. `None` means the provider did not report
    /// it — never zero, which would be a fabricated measurement.
    pub reported_tokens: Option<u64>,
}

impl AdaptiveSpend {
    pub fn is_balanced(self) -> bool {
        self.attempts == self.accepted.saturating_add(self.rejected)
    }
}

/// Durable adaptive state for exactly one run.
///
/// Absent (`None` on [`super::types::ComputerRun`]) means **no adaptive
/// authority**, not "no constraints": a record written before this field
/// existed deserializes to `None` and admits nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveRecord {
    pub profile: AdaptiveProfile,
    /// The exact spelling this record was ingested under, retained so an
    /// operator can see that an alias was used. Projected, never authoritative.
    pub ingested_as: String,
    pub spend: AdaptiveSpend,
    /// Consecutive proposals that repeated an already-seen (observation,
    /// action) pair. Durable so a restart cannot reset a no-progress loop.
    pub stationary_strikes: u32,
    /// Digest of the last accepted action. Host-private: it is a digest over
    /// model-authored text, so it is durable but never projected.
    #[serde(default)]
    pub last_action_digest: Option<String>,
    pub opened_at: DateTime<Utc>,
}

impl AdaptiveRecord {
    pub fn open(profile: AdaptiveProfile, ingested_as: &str, now: DateTime<Utc>) -> Self {
        Self {
            profile,
            ingested_as: ingested_as.to_string(),
            spend: AdaptiveSpend::default(),
            stationary_strikes: 0,
            last_action_digest: None,
            opened_at: now,
        }
    }

    pub fn budget_exhausted(&self) -> bool {
        self.spend.attempts >= self.profile.budget().max_model_turns
    }
}

/// What a packaged or virtualized Computer Use claim may say.
///
/// Ordered deliberately: `Unavailable` is the floor and the only verdict this
/// build can reach. Nothing in this crate observed a signed helper, a TCC
/// grant, a Virtualization.framework boot, a guest image, a captured frame, or
/// dispatched input, so a higher verdict would be a fabricated claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationVerdict {
    Unavailable,
    Partial,
    Pass,
}

/// A fail-closed packaged/VM qualification claim.
///
/// There is deliberately **no** public constructor that yields `Partial` or
/// `Pass`. Both require host-observed evidence this build cannot produce, so
/// the type makes the claim unreachable rather than leaving it to a reviewer
/// to notice that a simulator set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedQualification {
    pub verdict: QualificationVerdict,
    /// Named, closed reasons. Never a path, a bundle location, or a team ID.
    pub reasons: Vec<UnavailableReason>,
}

/// Why a packaged/VM verdict is `Unavailable`. A closed enum so a reason can
/// never carry an observed string, a filesystem path, or a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    NoSignedHelperInspected,
    NoOperatorTrustRoot,
    NoTccGrantObserved,
    NoVirtualizationBoot,
    NoGuestImageVerified,
    NoFrameCaptured,
    SimulatorBackend,
}

impl PackagedQualification {
    /// The only verdict reachable from source. Callers name what was missing;
    /// the verdict itself is not a caller decision.
    pub fn unavailable(reasons: impl IntoIterator<Item = UnavailableReason>) -> Self {
        let mut reasons: Vec<_> = reasons.into_iter().collect();
        reasons.sort();
        reasons.dedup();
        if reasons.is_empty() {
            reasons.push(UnavailableReason::NoSignedHelperInspected);
        }
        Self {
            verdict: QualificationVerdict::Unavailable,
            reasons,
        }
    }

    /// A simulator can never qualify a packaged build, whatever else it did.
    pub fn from_simulator() -> Self {
        Self::unavailable([
            UnavailableReason::SimulatorBackend,
            UnavailableReason::NoSignedHelperInspected,
            UnavailableReason::NoTccGrantObserved,
            UnavailableReason::NoVirtualizationBoot,
            UnavailableReason::NoFrameCaptured,
        ])
    }

    pub fn is_qualified(&self) -> bool {
        self.verdict != QualificationVerdict::Unavailable
    }
}

/// Redaction-safe adaptive view for the cockpit and any coordinator surface.
///
/// Carries no challenge, no digest of a secret, no element label or value, no
/// geometry, no evidence token, and no filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveProfileProjection {
    pub profile: AdaptiveProfile,
    pub ingested_alias: bool,
    pub budget: AdaptiveBudget,
    pub spend: AdaptiveSpend,
    pub stationary_strikes: u32,
    pub budget_exhausted: bool,
}

impl AdaptiveProfileProjection {
    pub fn of(record: &AdaptiveRecord) -> Self {
        Self {
            profile: record.profile,
            ingested_alias: AdaptiveProfile::is_ingest_alias(&record.ingested_as),
            budget: record.profile.budget(),
            spend: record.spend,
            stationary_strikes: record.stationary_strikes,
            budget_exhausted: record.budget_exhausted(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_ingest_only_and_never_serialize() {
        for (alias, canonical) in [
            ("efficient", AdaptiveProfile::Economy),
            ("frontier", AdaptiveProfile::HighAssurance),
        ] {
            let parsed: AdaptiveProfile =
                serde_json::from_str(&format!("\"{alias}\"")).expect("alias deserializes");
            assert_eq!(parsed, canonical);
            let rendered = serde_json::to_string(&parsed).unwrap();
            assert_eq!(rendered, format!("\"{}\"", canonical.canonical_str()));
            assert_ne!(rendered, format!("\"{alias}\""));
        }
    }

    #[test]
    fn an_alias_resolves_to_a_byte_identical_budget() {
        assert_eq!(
            AdaptiveProfile::ingest("efficient").unwrap().budget(),
            AdaptiveProfile::Economy.budget()
        );
        assert_eq!(
            AdaptiveProfile::ingest("frontier").unwrap().budget(),
            AdaptiveProfile::HighAssurance.budget()
        );
    }

    #[test]
    fn unknown_profile_names_fail_closed() {
        for name in ["", "eco", "ECONOMY", "unsafe", "high-assurance"] {
            assert_eq!(
                AdaptiveProfile::ingest(name).unwrap_err().code,
                ComputerErrorCode::InvalidRequest,
                "{name} must not resolve"
            );
        }
    }

    #[test]
    fn budgets_are_monotone_across_the_vocabulary() {
        let economy = AdaptiveProfile::Economy.budget();
        let balanced = AdaptiveProfile::Balanced.budget();
        let high = AdaptiveProfile::HighAssurance.budget();
        assert!(economy.is_within(balanced));
        assert!(balanced.is_within(high));
        assert!(!high.is_within(economy));
        assert_eq!(economy.max_image_bytes, 0, "economy sends no pixels");
    }

    #[test]
    fn budgets_never_encode_a_safety_relaxation() {
        // Every budget field must be a cost bound. If a field is ever added
        // that names a safety check, this assertion is the tripwire: the
        // serialized key set is pinned.
        let keys: Vec<String> =
            match serde_json::to_value(AdaptiveProfile::Economy.budget()).unwrap() {
                serde_json::Value::Object(map) => map.keys().cloned().collect(),
                other => panic!("budget must serialize as an object, got {other:?}"),
            };
        assert_eq!(
            keys,
            vec![
                "maxImageBytes".to_string(),
                "maxModelTurns".to_string(),
                "maxRepairs".to_string(),
                "maxTurnMillis".to_string(),
            ]
        );
    }

    #[test]
    fn a_simulator_can_never_report_a_packaged_pass() {
        let qualification = PackagedQualification::from_simulator();
        assert_eq!(qualification.verdict, QualificationVerdict::Unavailable);
        assert!(!qualification.is_qualified());
        assert!(qualification
            .reasons
            .contains(&UnavailableReason::SimulatorBackend));
    }

    #[test]
    fn qualification_reasons_are_a_closed_enum_with_no_free_text() {
        let rendered = serde_json::to_string(&PackagedQualification::from_simulator()).unwrap();
        assert!(!rendered.contains('/'), "{rendered} must carry no path");
        assert!(!rendered.contains('\\'), "{rendered} must carry no path");
        assert!(rendered.contains("\"unavailable\""));
    }

    #[test]
    fn an_empty_reason_list_still_fails_closed() {
        let qualification = PackagedQualification::unavailable([]);
        assert_eq!(qualification.verdict, QualificationVerdict::Unavailable);
        assert!(!qualification.reasons.is_empty());
    }

    #[test]
    fn spend_accounting_stays_balanced() {
        let spend = AdaptiveSpend {
            attempts: 3,
            accepted: 1,
            rejected: 2,
            image_bytes: 0,
            reported_tokens: None,
        };
        assert!(spend.is_balanced());
        assert!(!AdaptiveSpend {
            attempts: 3,
            accepted: 1,
            rejected: 1,
            ..spend
        }
        .is_balanced());
    }

    #[test]
    fn a_legacy_record_without_the_field_is_absent_not_permissive() {
        #[derive(Deserialize)]
        struct Legacy {
            #[serde(default)]
            adaptive: Option<AdaptiveRecord>,
        }
        let legacy: Legacy = serde_json::from_str("{}").unwrap();
        assert!(legacy.adaptive.is_none());
    }
}
