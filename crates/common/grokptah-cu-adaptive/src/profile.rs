//! Efficiency profiles.
//!
//! A profile decides how much a run is willing to *spend* to be sure: how
//! often it re-observes, whether it captures a region digest, how many times
//! it retries, how far it will hand work upward, and how high it sets its
//! confidence floors.
//!
//! A profile decides nothing about what a run is *allowed* to do. The
//! refusals in [`AUTHORITY_INVARIANTS`] fire identically under all three
//! profiles and at every model tier, and no profile field can reach them.
//! That separation is what makes running one benchmark across three profiles
//! meaningful: `Economy` is allowed to be worse at the task, and is not
//! allowed to be less safe. `tests/cu_adaptive_authority_parity.rs` asserts
//! it by driving every hazard scenario under every profile and comparing the
//! refusal sets exactly.
//!
//! The knobs are also ordered. Every numeric field is monotone across
//! `Economy -> Balanced -> HighAssurance` in the direction of more
//! verification, which is checked in this module's tests. A profile that was
//! stricter about one thing and looser about another would make "which
//! profile is safer" unanswerable.

use serde::{Deserialize, Serialize};

use crate::confidence::DecisionThresholds;
use crate::grounding::GroundingLevel;
use crate::schema::IntentFamily;
use crate::vocabulary::DenyReason;

/// Refusals that fire regardless of profile, tier, budget, or human
/// availability.
///
/// This is the authority boundary written down. Everything on this list is a
/// property of the world or of the grant -- the surface is secure, the frame
/// moved, the lease is gone, the class is outside the grant -- and none of it
/// is negotiable by spending more. A profile that could suppress one of these
/// would be buying authority, which no profile may do.
pub const AUTHORITY_INVARIANTS: &[DenyReason] = &[
    DenyReason::SensitiveSurface,
    DenyReason::RedactionRequired,
    DenyReason::StaleFrame,
    DenyReason::FrameEpochChanged,
    DenyReason::TargetDrifted,
    DenyReason::TargetMissing,
    DenyReason::ElementDisabled,
    DenyReason::ActionNotAdvertised,
    DenyReason::ClassOutsideGrant,
    DenyReason::PointerWithoutVisualGrounding,
    DenyReason::LeaseLost,
    DenyReason::LeaseVersionConflict,
    DenyReason::Cancelled,
    DenyReason::SchemaViolation,
    DenyReason::ApprovalDenied,
];

/// One of the three efficiency profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileId {
    /// Cheapest. Fewest observations, no region capture, shallow verification.
    Economy,
    /// The default. Re-observes before mutating and verifies postconditions.
    Balanced,
    /// Most expensive. Region capture on every step, bracketed evidence, the
    /// highest confidence floors, and no unattended low-confidence commits.
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

    /// The profile's settings.
    #[must_use]
    pub fn spec(self) -> ExecutionProfile {
        match self {
            Self::Economy => ExecutionProfile {
                id: self,
                region_policy: RegionPolicy::Never,
                reobserve_before_mutation: false,
                verify_postcondition: false,
                bracket_evidence: false,
                max_frame_age_millis: 10_000,
                max_retries_per_step: 1,
                max_retries_per_run: 4,
                max_escalations_per_run: 1,
                pointer_grounding_floor: GroundingLevel::SemanticPlusRegion,
                semantic_grounding_floor: GroundingLevel::Semantic,
                thresholds: DecisionThresholds {
                    abstain_below_bps: 1_500,
                    escalate_below_bps: 4_000,
                    commit_floor_bps: 6_000,
                    irreversible_commit_floor_bps: 9_000,
                    min_margin_bps: 500,
                    max_candidates: 3,
                    allow_low_confidence_with_approval: true,
                },
            },
            Self::Balanced => ExecutionProfile {
                id: self,
                region_policy: RegionPolicy::OnUncertainty,
                reobserve_before_mutation: true,
                verify_postcondition: true,
                bracket_evidence: false,
                max_frame_age_millis: 5_000,
                max_retries_per_step: 2,
                max_retries_per_run: 8,
                max_escalations_per_run: 2,
                pointer_grounding_floor: GroundingLevel::SemanticPlusRegion,
                semantic_grounding_floor: GroundingLevel::Semantic,
                thresholds: DecisionThresholds {
                    abstain_below_bps: 2_000,
                    escalate_below_bps: 5_000,
                    commit_floor_bps: 7_000,
                    irreversible_commit_floor_bps: 9_200,
                    min_margin_bps: 1_000,
                    max_candidates: 2,
                    allow_low_confidence_with_approval: true,
                },
            },
            Self::HighAssurance => ExecutionProfile {
                id: self,
                region_policy: RegionPolicy::EveryStep,
                reobserve_before_mutation: true,
                verify_postcondition: true,
                bracket_evidence: true,
                max_frame_age_millis: 2_000,
                max_retries_per_step: 3,
                max_retries_per_run: 12,
                max_escalations_per_run: 3,
                pointer_grounding_floor: GroundingLevel::SemanticPlusRegion,
                semantic_grounding_floor: GroundingLevel::SemanticPlusRegion,
                thresholds: DecisionThresholds {
                    abstain_below_bps: 2_500,
                    escalate_below_bps: 6_000,
                    commit_floor_bps: 8_000,
                    irreversible_commit_floor_bps: 9_500,
                    min_margin_bps: 1_500,
                    max_candidates: 1,
                    // The most expensive profile refuses rather than asking a
                    // human to underwrite a guess.
                    allow_low_confidence_with_approval: false,
                },
            },
        }
    }
}

/// When a region digest is captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionPolicy {
    /// Semantic tree only. Cheapest, and blind to anything the tree does not
    /// say.
    Never,
    /// Captured when the previous step ended uncertain.
    OnUncertainty,
    /// Captured with every observation.
    EveryStep,
}

/// The settings one profile applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionProfile {
    pub id: ProfileId,
    pub region_policy: RegionPolicy,
    /// Re-observe immediately before dispatching a mutating step.
    pub reobserve_before_mutation: bool,
    /// Re-observe after a mutating step and check the postcondition rather
    /// than trusting the reported outcome.
    pub verify_postcondition: bool,
    /// Record both the pre- and post-action frame digests, so a reviewer can
    /// replay the mutation from the receipt.
    pub bracket_evidence: bool,
    /// The oldest frame a step may reference. Never above the kernel's own
    /// ceiling; see [`MAX_FRAME_AGE_CEILING_MILLIS`].
    pub max_frame_age_millis: u64,
    pub max_retries_per_step: u32,
    pub max_retries_per_run: u32,
    pub max_escalations_per_run: u32,
    pointer_grounding_floor: GroundingLevel,
    semantic_grounding_floor: GroundingLevel,
    pub thresholds: DecisionThresholds,
}

/// The hard ceiling on frame age, mirroring the kernel's own bound. No profile
/// may sit above it; the checked constructor path refuses one that does.
pub const MAX_FRAME_AGE_CEILING_MILLIS: u64 = 10_000;

impl ExecutionProfile {
    /// The grounding floor this profile imposes on a family of intents.
    ///
    /// This is a floor, not a setting: [`crate::grounding::required_level`]
    /// takes the maximum of it and the intent's intrinsic requirement, so a
    /// profile can only ever raise the bar.
    #[must_use]
    pub fn grounding_floor_for(&self, family: IntentFamily) -> GroundingLevel {
        match family {
            IntentFamily::Ambient | IntentFamily::KeyChord => GroundingLevel::None,
            IntentFamily::Semantic | IntentFamily::TextEntry => self.semantic_grounding_floor,
            IntentFamily::PointerFallback => self.pointer_grounding_floor,
        }
    }

    /// True when the profile is internally coherent and inside the kernel's
    /// ceilings.
    #[must_use]
    pub fn is_coherent(&self) -> bool {
        self.thresholds.is_coherent()
            && self.max_frame_age_millis > 0
            && self.max_frame_age_millis <= MAX_FRAME_AGE_CEILING_MILLIS
            && self.max_retries_per_step <= self.max_retries_per_run
            && self.pointer_grounding_floor >= GroundingLevel::SemanticPlusRegion
    }

    /// The refusals this profile can never suppress. Identical for every
    /// profile, by construction: the list is a constant, not a field.
    #[must_use]
    pub fn unconditional_denials(&self) -> &'static [DenyReason] {
        AUTHORITY_INVARIANTS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_is_coherent() {
        for id in ProfileId::ALL {
            let spec = id.spec();
            assert!(spec.is_coherent(), "{id:?} is incoherent");
            assert_eq!(spec.id, *id);
        }
    }

    #[test]
    fn profiles_are_monotone_in_verification() {
        let economy = ProfileId::Economy.spec();
        let balanced = ProfileId::Balanced.spec();
        let assured = ProfileId::HighAssurance.spec();
        for (cheaper, dearer) in [(economy, balanced), (balanced, assured)] {
            assert!(dearer.region_policy >= cheaper.region_policy);
            assert!(dearer.max_frame_age_millis <= cheaper.max_frame_age_millis);
            assert!(dearer.max_retries_per_step >= cheaper.max_retries_per_step);
            assert!(dearer.max_escalations_per_run >= cheaper.max_escalations_per_run);
            assert!(dearer.thresholds.commit_floor_bps >= cheaper.thresholds.commit_floor_bps);
            assert!(dearer.thresholds.min_margin_bps >= cheaper.thresholds.min_margin_bps);
            assert!(dearer.thresholds.max_candidates <= cheaper.thresholds.max_candidates);
            // `false < true`, so this is the monotonicity claim for the
            // boolean knobs: a dearer profile never turns verification off.
            assert!(dearer.reobserve_before_mutation >= cheaper.reobserve_before_mutation);
            assert!(dearer.verify_postcondition >= cheaper.verify_postcondition);
            assert!(dearer.bracket_evidence >= cheaper.bracket_evidence);
        }
        assert!(!economy.verify_postcondition);
        assert!(balanced.verify_postcondition);
        assert!(assured.bracket_evidence);
    }

    #[test]
    fn no_profile_can_suppress_an_authority_invariant() {
        let baseline = ProfileId::Economy.spec().unconditional_denials();
        for id in ProfileId::ALL {
            assert_eq!(id.spec().unconditional_denials(), baseline);
        }
        // The list is a constant reachable from no profile field, so there is
        // no configuration that shortens it.
        assert!(AUTHORITY_INVARIANTS.contains(&DenyReason::SensitiveSurface));
        assert!(AUTHORITY_INVARIANTS.contains(&DenyReason::PointerWithoutVisualGrounding));
        assert!(AUTHORITY_INVARIANTS.contains(&DenyReason::LeaseVersionConflict));
    }

    #[test]
    fn no_profile_sits_above_the_kernel_frame_age_ceiling() {
        for id in ProfileId::ALL {
            assert!(id.spec().max_frame_age_millis <= MAX_FRAME_AGE_CEILING_MILLIS);
        }
    }

    #[test]
    fn pointer_floors_are_identical_across_profiles() {
        // Cost buys verification. It does not buy a cheaper pointer rule.
        for id in ProfileId::ALL {
            assert_eq!(
                id.spec().grounding_floor_for(IntentFamily::PointerFallback),
                GroundingLevel::SemanticPlusRegion
            );
        }
    }
}
