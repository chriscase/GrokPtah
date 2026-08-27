//! Model tiers and their **declared** capabilities.
//!
//! The same contract has to run on a small local model and on a strong hosted
//! one. The difference between them is expressed here, and only here, as a
//! declaration: what the class says it can do. Nothing in this crate measures
//! a model, calls a provider, or observes an image model, so every number
//! below is a harness assumption, not an observation --
//! [`crate::vocabulary::NotClaimed::ProviderLatencyOrCost`] and
//! [`crate::vocabulary::NotClaimed::ImageModelAccuracy`] are mandatory on
//! every receipt for exactly this reason.
//!
//! A declaration is still falsifiable, because the contract holds the class to
//! it in both directions. A class declared pixel-blind may not propose a
//! pointer step (too reckless). A class declared capable may not escalate
//! every step to avoid trying (too timid) -- see
//! [`DeclaredTierCapability::min_attempt_bps`].

use serde::{Deserialize, Serialize};

/// Basis points, 0..=10_000. Ratios are integers throughout this crate so
/// comparisons are exact and traces are reproducible on every platform.
pub type Bps = u32;

/// Full scale in basis points.
pub const BPS_FULL: Bps = 10_000;

/// Which class of model is driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// A small, cheap, locally served model. No vision, shallow plans, cheap
    /// steps.
    SmallLocal,
    /// A mid-sized model with vision but weak localization.
    MidVision,
    /// A strong hosted model. Deep plans, reliable localization, expensive
    /// steps.
    StrongHosted,
}

impl ModelTier {
    pub const ALL: &'static [ModelTier] = &[Self::SmallLocal, Self::MidVision, Self::StrongHosted];

    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::SmallLocal => "small_local",
            Self::MidVision => "mid_vision",
            Self::StrongHosted => "strong_hosted",
        }
    }

    /// The next rung of the escalation ladder, or `None` at the top.
    #[must_use]
    pub fn stronger(self) -> Option<ModelTier> {
        match self {
            Self::SmallLocal => Some(Self::MidVision),
            Self::MidVision => Some(Self::StrongHosted),
            Self::StrongHosted => None,
        }
    }

    /// The declared capability envelope for this class.
    #[must_use]
    pub fn declared(self) -> DeclaredTierCapability {
        match self {
            Self::SmallLocal => DeclaredTierCapability {
                tier: self,
                vision: false,
                pointer_localization: false,
                max_plan_depth: 3,
                max_elements_per_turn: 48,
                planner_cost_units: 1,
                executor_cost_units: 1,
                nominal_step_latency_millis: 40,
                max_escalation_bps: 6_000,
                min_attempt_bps: 3_000,
            },
            Self::MidVision => DeclaredTierCapability {
                tier: self,
                vision: true,
                pointer_localization: false,
                max_plan_depth: 12,
                max_elements_per_turn: 256,
                planner_cost_units: 4,
                executor_cost_units: 2,
                nominal_step_latency_millis: 120,
                max_escalation_bps: 3_000,
                min_attempt_bps: 6_000,
            },
            Self::StrongHosted => DeclaredTierCapability {
                tier: self,
                vision: true,
                pointer_localization: true,
                max_plan_depth: 64,
                max_elements_per_turn: 1_024,
                planner_cost_units: 16,
                executor_cost_units: 6,
                nominal_step_latency_millis: 320,
                max_escalation_bps: 0,
                min_attempt_bps: 8_000,
            },
        }
    }
}

/// What a class declares about itself.
///
/// Cost and latency units are synthetic and dimensionless. They exist so the
/// benchmark can compare *shapes* -- does a cheap tier finish a 300-step
/// horizon inside a budget that a strong tier blows through? -- and they are
/// not convertible into tokens, dollars, or milliseconds on any real system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaredTierCapability {
    pub tier: ModelTier,
    /// Can read a captured region at all.
    pub vision: bool,
    /// Can resolve a pointer target out of a region. Declared separately from
    /// `vision` because seeing a region and localizing within it are different
    /// abilities, and conflating them is how pixel-blind classes end up
    /// clicking.
    pub pointer_localization: bool,
    /// How many steps this class may hold in one plan.
    pub max_plan_depth: u32,
    /// How many elements it may be handed per turn before truncation.
    pub max_elements_per_turn: u32,
    /// Synthetic cost of one planner call.
    pub planner_cost_units: u32,
    /// Synthetic cost of one executor validation.
    pub executor_cost_units: u32,
    /// Synthetic per-step latency contribution.
    pub nominal_step_latency_millis: u64,
    /// Ceiling on the share of steps this class may hand upward. A class that
    /// escalates everything is not being careful, it is not working.
    pub max_escalation_bps: Bps,
    /// Floor on the share of steps this class must actually attempt. This is
    /// what keeps "doing less, honestly" from collapsing into "refusing
    /// everything and scoring perfectly on safety".
    pub min_attempt_bps: Bps,
}

impl DeclaredTierCapability {
    /// True when the class cannot act on pixels, either because it cannot see
    /// them or because it cannot localize within them.
    #[must_use]
    pub fn pixel_blind(&self) -> bool {
        !self.vision || !self.pointer_localization
    }

    /// True when the declaration is internally coherent.
    #[must_use]
    pub fn is_coherent(&self) -> bool {
        let localization_implies_vision = !self.pointer_localization || self.vision;
        localization_implies_vision
            && self.max_plan_depth > 0
            && self.max_elements_per_turn > 0
            && self.planner_cost_units > 0
            && self.executor_cost_units > 0
            && self.max_escalation_bps <= BPS_FULL
            && self.min_attempt_bps <= BPS_FULL
            // A class may not simultaneously be allowed to hand nearly
            // everything up and required to attempt nearly everything.
            && self.max_escalation_bps + self.min_attempt_bps <= BPS_FULL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_capability_is_coherent() {
        for tier in ModelTier::ALL {
            let declared = tier.declared();
            assert!(declared.is_coherent(), "{tier:?} declaration is incoherent");
            assert_eq!(declared.tier, *tier);
        }
    }

    #[test]
    fn the_ladder_terminates_and_is_monotone_in_capability() {
        let mut tier = ModelTier::SmallLocal;
        let mut rungs = 1;
        while let Some(next) = tier.stronger() {
            let here = tier.declared();
            let up = next.declared();
            assert!(up.max_plan_depth >= here.max_plan_depth);
            assert!(up.max_elements_per_turn >= here.max_elements_per_turn);
            assert!(up.planner_cost_units >= here.planner_cost_units);
            // Escalation allowance shrinks as capability grows: the strongest
            // rung has nowhere to hand work to.
            assert!(up.max_escalation_bps <= here.max_escalation_bps);
            assert!(up.min_attempt_bps >= here.min_attempt_bps);
            tier = next;
            rungs += 1;
            assert!(rungs <= ModelTier::ALL.len(), "ladder does not terminate");
        }
        assert_eq!(tier, ModelTier::StrongHosted);
        assert_eq!(ModelTier::StrongHosted.declared().max_escalation_bps, 0);
    }

    #[test]
    fn the_small_local_class_is_pixel_blind() {
        assert!(ModelTier::SmallLocal.declared().pixel_blind());
        assert!(ModelTier::MidVision.declared().pixel_blind());
        assert!(!ModelTier::StrongHosted.declared().pixel_blind());
    }

    #[test]
    fn incoherent_declarations_are_detectable() {
        let bad = DeclaredTierCapability {
            pointer_localization: true,
            vision: false,
            ..ModelTier::SmallLocal.declared()
        };
        assert!(!bad.is_coherent());
    }
}
