//! Execution profiles.
//!
//! A profile buys verification with cost. It never buys authority.
//!
//! Everything a profile controls is on the *spending* side: how often to
//! re-observe, whether to capture a screenshot digest, how many steps and
//! tokens to allow, how hard to verify a postcondition. Nothing a profile
//! controls can widen what the agent is allowed to do or see. The guard's
//! invariants (`invariant::AUTHORITY_INVARIANTS`) are evaluated identically
//! under all three, and `tests/cu_bench_authority_parity.rs` asserts it.
//!
//! That separation is the whole point of running one benchmark across three
//! profiles: economy is allowed to be worse at the task, and is not allowed
//! to be less safe.

use serde::{Deserialize, Serialize};

/// How often a screenshot digest is captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotPolicy {
    /// Semantic tree only. Cheapest, and blind to the ambiguous-pixel family.
    Never,
    /// Captured when the previous step ended uncertain, or when the model
    /// asked for one.
    OnUncertainty,
    /// Captured with every observation.
    EveryStep,
}

/// How much evidence an executed action must leave behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    /// Action digest and outcome.
    Minimal,
    /// Adds the pre-action observation digest.
    Linked,
    /// Adds the post-action observation digest, so every mutation is
    /// bracketed by two observations that a reviewer can replay.
    Bracketed,
}

/// One of the three execution profiles under comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileId {
    Economy,
    Balanced,
    HighAssurance,
}

impl ProfileId {
    pub const ALL: &'static [ProfileId] = &[Self::Economy, Self::Balanced, Self::HighAssurance];

    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::HighAssurance => "high_assurance",
        }
    }
}

/// The knobs a profile sets. Cost and verification only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionProfile {
    pub id: ProfileId,
    pub screenshot_policy: ScreenshotPolicy,
    /// Re-observe immediately before dispatching any mutating action.
    pub reobserve_before_mutation: bool,
    /// Re-observe immediately after a mutating action and check the
    /// postcondition rather than trusting the reported outcome.
    pub verify_postcondition: bool,
    /// Maximum age of the observation an action may reference. Tighter than
    /// the production default on high assurance, never looser than the
    /// production ceiling.
    pub max_observation_age_millis: u64,
    pub max_steps: u32,
    pub max_retries_per_action: u32,
    /// Wall-clock-equivalent budget in virtual milliseconds.
    pub latency_budget_millis: u64,
    /// Total modeled tokens (prompt + completion) across the run.
    pub token_budget: u32,
    pub evidence_level: EvidenceLevel,
    /// Whether a pointer click may be used when no semantic path exists.
    /// Independent of authority: a pointer click still needs its own grant
    /// class, at every profile.
    pub pointer_fallback_enabled: bool,
}

impl ExecutionProfile {
    #[must_use]
    pub fn economy() -> Self {
        Self {
            id: ProfileId::Economy,
            screenshot_policy: ScreenshotPolicy::Never,
            reobserve_before_mutation: false,
            verify_postcondition: false,
            max_observation_age_millis: 10_000,
            max_steps: 24,
            max_retries_per_action: 1,
            latency_budget_millis: 60_000,
            token_budget: 24_000,
            evidence_level: EvidenceLevel::Minimal,
            pointer_fallback_enabled: false,
        }
    }

    #[must_use]
    pub fn balanced() -> Self {
        Self {
            id: ProfileId::Balanced,
            screenshot_policy: ScreenshotPolicy::OnUncertainty,
            reobserve_before_mutation: true,
            verify_postcondition: true,
            max_observation_age_millis: 5_000,
            max_steps: 40,
            max_retries_per_action: 2,
            latency_budget_millis: 180_000,
            token_budget: 90_000,
            evidence_level: EvidenceLevel::Linked,
            pointer_fallback_enabled: true,
        }
    }

    #[must_use]
    pub fn high_assurance() -> Self {
        Self {
            id: ProfileId::HighAssurance,
            screenshot_policy: ScreenshotPolicy::EveryStep,
            reobserve_before_mutation: true,
            verify_postcondition: true,
            max_observation_age_millis: 1_500,
            max_steps: 64,
            max_retries_per_action: 2,
            latency_budget_millis: 480_000,
            token_budget: 260_000,
            evidence_level: EvidenceLevel::Bracketed,
            pointer_fallback_enabled: true,
        }
    }

    #[must_use]
    pub fn for_id(id: ProfileId) -> Self {
        match id {
            ProfileId::Economy => Self::economy(),
            ProfileId::Balanced => Self::balanced(),
            ProfileId::HighAssurance => Self::high_assurance(),
        }
    }

    /// The production `ComputerUseLimits::ceiling()` values this benchmark
    /// must stay inside. A profile that exceeded them would be measuring a
    /// configuration production refuses to run.
    #[must_use]
    pub fn within_production_ceiling(&self) -> bool {
        self.max_steps <= 256
            && self.max_retries_per_action <= 5
            && self.max_observation_age_millis > 0
            && self.max_observation_age_millis <= 60_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_profiles_stay_inside_the_production_ceiling() {
        for id in ProfileId::ALL {
            let profile = ExecutionProfile::for_id(*id);
            assert!(
                profile.within_production_ceiling(),
                "{} exceeds the production limit ceiling",
                id.slug()
            );
        }
    }

    #[test]
    fn verification_depth_is_monotonic_across_profiles() {
        let economy = ExecutionProfile::economy();
        let balanced = ExecutionProfile::balanced();
        let assurance = ExecutionProfile::high_assurance();

        assert!(economy.screenshot_policy < balanced.screenshot_policy);
        assert!(balanced.screenshot_policy < assurance.screenshot_policy);
        assert!(economy.evidence_level < balanced.evidence_level);
        assert!(balanced.evidence_level < assurance.evidence_level);
        assert!(economy.token_budget < balanced.token_budget);
        assert!(balanced.token_budget < assurance.token_budget);
        // Freshness gets stricter, not looser, as assurance rises.
        assert!(economy.max_observation_age_millis > balanced.max_observation_age_millis);
        assert!(balanced.max_observation_age_millis > assurance.max_observation_age_millis);
    }

    #[test]
    fn profile_ids_have_unique_slugs() {
        let mut slugs: Vec<&str> = ProfileId::ALL.iter().map(|id| id.slug()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), ProfileId::ALL.len());
    }
}
