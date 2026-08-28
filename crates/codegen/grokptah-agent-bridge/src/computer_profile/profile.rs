//! Canonical adaptive Computer Use profiles and efficiency budgets.
//!
//! A profile controls how much work an already-authorized Computer Run may
//! spend. It is not an authorization level. The Computer Use policy kernel
//! remains the only authority for target, consent, leases, freshness,
//! sensitivity, retries, and dispatch.

use serde::{Deserialize, Serialize};

/// The only product profile identifiers. Compatibility tokens are accepted
/// while ingesting old records and are never emitted.
pub const CANONICAL_PROFILE_NAMES: [&str; 3] = ["economy", "balanced", "high_assurance"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileTokenKind {
    Canonical,
    CompatibilityAlias,
}

/// One of the three canonical execution profiles.
///
/// Ordering is the escalation order. This ordering never grants authority; it
/// only makes it possible to prove that a transition moves one rung upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum AdaptiveProfile {
    #[default]
    Economy,
    Balanced,
    HighAssurance,
}

impl AdaptiveProfile {
    pub const ALL: [Self; 3] = [Self::Economy, Self::Balanced, Self::HighAssurance];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::HighAssurance => "high_assurance",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Economy => "Economy",
            Self::Balanced => "Balanced",
            Self::HighAssurance => "High Assurance",
        }
    }

    pub const fn escalated(self) -> Option<Self> {
        match self {
            Self::Economy => Some(Self::Balanced),
            Self::Balanced => Some(Self::HighAssurance),
            Self::HighAssurance => None,
        }
    }

    /// Parse a persisted or operator-supplied token. This is intentionally
    /// case-sensitive so malformed configuration does not get normalized into
    /// an authority-bearing value.
    pub fn from_ingest_token(token: &str) -> Option<(Self, ProfileTokenKind)> {
        match token {
            "economy" => Some((Self::Economy, ProfileTokenKind::Canonical)),
            "balanced" => Some((Self::Balanced, ProfileTokenKind::Canonical)),
            "high_assurance" => Some((Self::HighAssurance, ProfileTokenKind::Canonical)),
            "efficient" => Some((Self::Economy, ProfileTokenKind::CompatibilityAlias)),
            "frontier" => Some((Self::HighAssurance, ProfileTokenKind::CompatibilityAlias)),
            _ => None,
        }
    }

    pub const fn budget(self) -> ProfileBudget {
        match self {
            Self::Economy => ECONOMY_BUDGET,
            Self::Balanced => BALANCED_BUDGET,
            Self::HighAssurance => HIGH_ASSURANCE_BUDGET,
        }
    }

    /// High Assurance adds an independent-verifier obligation. No profile
    /// removes an obligation from the shared safety floor.
    pub const fn requires_independent_verifier(self) -> bool {
        matches!(self, Self::HighAssurance)
    }

    pub const fn safety_floor(self) -> SafetyFloor {
        let _ = self;
        SafetyFloor::REQUIRED
    }
}

impl std::fmt::Display for AdaptiveProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for AdaptiveProfile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AdaptiveProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let token = String::deserialize(deserializer)?;
        AdaptiveProfile::from_ingest_token(&token)
            .map(|(profile, _)| profile)
            .ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "unknown Computer Use profile {token:?}; expected economy, balanced, or high_assurance"
                ))
            })
    }
}

/// Detail available to the model. No variant permits raw frame bytes to cross
/// a public API boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
pub enum ObservationDetail {
    SemanticOnly = 0,
    SemanticWithGeometry = 1,
    SemanticWithEvidenceRef = 2,
}

impl ObservationDetail {
    pub const fn allows_geometry(self) -> bool {
        !matches!(self, Self::SemanticOnly)
    }

    pub const fn allows_evidence_reference(self) -> bool {
        matches!(self, Self::SemanticWithEvidenceRef)
    }

    /// Raw screenshot bytes are private adapter input, never public profile
    /// output. A visual adapter must enforce its own authenticated route.
    pub const fn allows_screenshot_bytes(self) -> bool {
        false
    }
}

/// Efficiency ceilings. This type intentionally contains no consent,
/// authority, freshness, redaction, or retry override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBudget {
    pub observation_detail: ObservationDetail,
    pub max_observation_elements: u32,
    pub max_observation_bytes: u64,
    pub max_element_text_bytes: u32,
    pub max_model_calls: u32,
    pub max_repairs: u32,
    pub max_response_bytes: u64,
    pub max_turn_millis: u64,
    pub max_text_entry_bytes: u32,
    pub max_scroll_delta: i32,
    pub max_summary_bytes: u32,
    pub allows_screenshot_capture: bool,
    pub allows_pointer_fallback: bool,
    pub allows_key_chord: bool,
}

/// Economy: semantic-first, compact, and screenshot-free.
pub const ECONOMY_BUDGET: ProfileBudget = ProfileBudget {
    observation_detail: ObservationDetail::SemanticOnly,
    max_observation_elements: 48,
    max_observation_bytes: 24 * 1024,
    max_element_text_bytes: 256,
    max_model_calls: 16,
    max_repairs: 1,
    max_response_bytes: 8 * 1024,
    max_turn_millis: 20_000,
    max_text_entry_bytes: 1_024,
    max_scroll_delta: 2_000,
    max_summary_bytes: 256,
    allows_screenshot_capture: false,
    allows_pointer_fallback: false,
    allows_key_chord: false,
};

/// Balanced: semantic-first with bounded geometry and recovery.
pub const BALANCED_BUDGET: ProfileBudget = ProfileBudget {
    observation_detail: ObservationDetail::SemanticWithGeometry,
    max_observation_elements: 256,
    max_observation_bytes: 128 * 1024,
    max_element_text_bytes: 512,
    max_model_calls: 48,
    max_repairs: 2,
    max_response_bytes: 32 * 1024,
    max_turn_millis: 60_000,
    max_text_entry_bytes: 4 * 1024,
    max_scroll_delta: 10_000,
    max_summary_bytes: 512,
    allows_screenshot_capture: true,
    allows_pointer_fallback: false,
    allows_key_chord: false,
};

/// High Assurance: richest eligible observation and additive visual
/// verification obligations. It never relaxes shared safety.
pub const HIGH_ASSURANCE_BUDGET: ProfileBudget = ProfileBudget {
    observation_detail: ObservationDetail::SemanticWithEvidenceRef,
    max_observation_elements: 1_024,
    max_observation_bytes: 512 * 1024,
    max_element_text_bytes: 512,
    max_model_calls: 96,
    max_repairs: 3,
    max_response_bytes: 128 * 1024,
    max_turn_millis: 120_000,
    max_text_entry_bytes: 16 * 1024,
    max_scroll_delta: 10_000,
    max_summary_bytes: 512,
    allows_screenshot_capture: true,
    allows_pointer_fallback: true,
    allows_key_chord: true,
};

/// Invariants that apply to all profiles. This is a single constant rather
/// than a per-profile table, making it impossible for a profile budget to
/// weaken the safety floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyFloor {
    pub requires_host_verification: bool,
    pub requires_fresh_observation_binding: bool,
    pub requires_completion_bound_to_current_observation: bool,
    pub allows_screenshot_bytes_to_model: bool,
    pub allows_free_form_action: bool,
    pub allows_automatic_replay_after_uncertain_dispatch: bool,
    pub max_stationary_repeats: u32,
    pub max_consecutive_uncertain_answers: u32,
    pub min_confidence_permille: u16,
    pub max_verification_failures: u32,
}

impl SafetyFloor {
    pub const REQUIRED: Self = Self {
        requires_host_verification: true,
        requires_fresh_observation_binding: true,
        requires_completion_bound_to_current_observation: true,
        allows_screenshot_bytes_to_model: false,
        allows_free_form_action: false,
        allows_automatic_replay_after_uncertain_dispatch: false,
        max_stationary_repeats: 2,
        max_consecutive_uncertain_answers: 2,
        min_confidence_permille: 700,
        max_verification_failures: 2,
    };
}

#[cfg(test)]
impl ProfileBudget {
    pub(crate) const fn default_for_test() -> Self {
        ECONOMY_BUDGET
    }
}

// Compile-time proofs that the adaptive layer can only narrow the existing
// provider-neutral kernel. The values intentionally mirror hard kernel limits.
const KERNEL_TEXT_ENTRY_BYTES: u32 = 16 * 1024;
const KERNEL_SEMANTIC_ELEMENTS: u32 = 10_000;
const KERNEL_SEMANTIC_BYTES: u64 = 8 * 1024 * 1024;
const KERNEL_RETRIES: u32 = 5;
const KERNEL_DURATION_MILLIS: u64 = 60 * 60 * 1_000;
const KERNEL_SCROLL: i32 = 10_000;

const fn within_kernel(budget: &ProfileBudget) -> bool {
    budget.max_observation_elements > 0
        && budget.max_observation_elements <= KERNEL_SEMANTIC_ELEMENTS
        && budget.max_observation_bytes > 0
        && budget.max_observation_bytes <= KERNEL_SEMANTIC_BYTES
        && budget.max_model_calls > 0
        && budget.max_repairs <= KERNEL_RETRIES
        && budget.max_turn_millis > 0
        && budget.max_turn_millis <= KERNEL_DURATION_MILLIS
        && budget.max_text_entry_bytes > 0
        && budget.max_text_entry_bytes <= KERNEL_TEXT_ENTRY_BYTES
        && budget.max_scroll_delta > 0
        && budget.max_scroll_delta <= KERNEL_SCROLL
}

const fn implies(lower: bool, higher: bool) -> bool {
    !lower || higher
}

const fn narrows(lower: &ProfileBudget, higher: &ProfileBudget) -> bool {
    (lower.observation_detail as u8) <= (higher.observation_detail as u8)
        && lower.max_observation_elements <= higher.max_observation_elements
        && lower.max_observation_bytes <= higher.max_observation_bytes
        && lower.max_element_text_bytes <= higher.max_element_text_bytes
        && lower.max_model_calls <= higher.max_model_calls
        && lower.max_repairs <= higher.max_repairs
        && lower.max_response_bytes <= higher.max_response_bytes
        && lower.max_turn_millis <= higher.max_turn_millis
        && lower.max_text_entry_bytes <= higher.max_text_entry_bytes
        && lower.max_scroll_delta <= higher.max_scroll_delta
        && lower.max_summary_bytes <= higher.max_summary_bytes
        && implies(
            lower.allows_screenshot_capture,
            higher.allows_screenshot_capture,
        )
        && implies(
            lower.allows_pointer_fallback,
            higher.allows_pointer_fallback,
        )
        && implies(lower.allows_key_chord, higher.allows_key_chord)
}

const _: () = assert!(
    within_kernel(&ECONOMY_BUDGET)
        && within_kernel(&BALANCED_BUDGET)
        && within_kernel(&HIGH_ASSURANCE_BUDGET)
);
const _: () = assert!(
    narrows(&ECONOMY_BUDGET, &BALANCED_BUDGET) && narrows(&BALANCED_BUDGET, &HIGH_ASSURANCE_BUDGET)
);
const _: () = assert!(
    !ECONOMY_BUDGET.observation_detail.allows_screenshot_bytes()
        && !BALANCED_BUDGET.observation_detail.allows_screenshot_bytes()
        && !HIGH_ASSURANCE_BUDGET
            .observation_detail
            .allows_screenshot_bytes()
);
const _: () = assert!(
    SafetyFloor::REQUIRED.requires_host_verification
        && SafetyFloor::REQUIRED.requires_fresh_observation_binding
        && SafetyFloor::REQUIRED.requires_completion_bound_to_current_observation
        && !SafetyFloor::REQUIRED.allows_screenshot_bytes_to_model
        && !SafetyFloor::REQUIRED.allows_free_form_action
        && !SafetyFloor::REQUIRED.allows_automatic_replay_after_uncertain_dispatch
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_ingest_only() {
        assert_eq!(
            AdaptiveProfile::from_ingest_token("efficient"),
            Some((
                AdaptiveProfile::Economy,
                ProfileTokenKind::CompatibilityAlias
            ))
        );
        assert_eq!(
            AdaptiveProfile::from_ingest_token("frontier"),
            Some((
                AdaptiveProfile::HighAssurance,
                ProfileTokenKind::CompatibilityAlias
            ))
        );
        for profile in AdaptiveProfile::ALL {
            let wire = serde_json::to_string(&profile).unwrap();
            assert!(wire.contains(profile.as_str()));
            assert!(!wire.contains("efficient"));
            assert!(!wire.contains("frontier"));
            assert_eq!(profile.safety_floor(), SafetyFloor::REQUIRED);
        }
        assert_eq!(
            serde_json::to_string(
                &serde_json::from_str::<AdaptiveProfile>("\"frontier\"").unwrap()
            )
            .unwrap(),
            "\"high_assurance\""
        );
    }

    #[test]
    fn malformed_profiles_fail_closed() {
        assert!(AdaptiveProfile::from_ingest_token("Economy").is_none());
        assert!(AdaptiveProfile::from_ingest_token("cheap").is_none());
        assert!(serde_json::from_str::<AdaptiveProfile>("\"ludicrous\"").is_err());
        assert!(serde_json::from_str::<AdaptiveProfile>("3").is_err());
        assert_eq!(AdaptiveProfile::ALL.len(), 3);
    }
}
