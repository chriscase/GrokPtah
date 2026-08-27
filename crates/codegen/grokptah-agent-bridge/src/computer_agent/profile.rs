//! Adaptive Computer Use profiles: one deterministic ceiling set per model class.
//!
//! Small local models and frontier models share one safety contract. They do
//! not share one *budget*: a cheap model needs a small, semantic-only context
//! and almost no room to flail, while a frontier model may be given geometry
//! and a redacted evidence reference. A profile is the only place those
//! numbers live, so a model class can never widen its own ceilings by
//! returning something clever.
//!
//! Every ceiling here is a constant. Nothing is derived from provider
//! metadata, model self-report, or the observation itself, so two runs of the
//! same profile always admit exactly the same inputs.
//!
//! Profiles narrow; they never widen. [`ModelBoundaryCeilings::validate`]
//! proves that against [`ComputerUseLimits::ceiling`], and the unit tests
//! below run it for every profile.

use serde::{Deserialize, Serialize};

use crate::computer_use::ComputerUseLimits;
use crate::gateway_config::ComputerUseTier;

/// How much of an observation a profile may put in front of the model.
///
/// No variant admits screenshot *bytes*. Frontier is allowed the redacted
/// evidence reference (hash, media type, dimensions) because that is already
/// non-reversible metadata; the pixels stay host-side in every profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationDetail {
    /// Semantic elements only. No geometry, no evidence reference, no bytes.
    /// This is the "no raw accessibility tree" shape: role, label, value,
    /// enabled/focused, and the advertised action set, and nothing else.
    SemanticOnly,
    /// Adds per-element geometry, which is enough to reconstruct layout.
    SemanticWithGeometry,
    /// Adds the redacted screenshot reference. Never the screenshot itself.
    SemanticWithEvidenceRef,
}

impl ObservationDetail {
    pub fn allows_geometry(self) -> bool {
        self >= Self::SemanticWithGeometry
    }

    pub fn allows_evidence_reference(self) -> bool {
        self >= Self::SemanticWithEvidenceRef
    }

    /// Screenshot bytes never cross the model boundary in any profile.
    pub fn allows_screenshot_bytes(self) -> bool {
        false
    }
}

/// Deterministic per-profile ceilings for context, tokens, time, and retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBoundaryCeilings {
    pub observation_detail: ObservationDetail,
    /// Elements offered to the model. Beyond this the observation is refused
    /// rather than silently trimmed: a trimmed observation would let the model
    /// act on a view the operator never saw.
    pub max_observation_elements: u32,
    /// Serialized bytes of the rendered observation payload.
    pub max_observation_bytes: u64,
    /// Per-element label/value bytes in the rendered payload.
    pub max_element_text_bytes: u32,
    /// Provider-reported prompt tokens, when the provider reports usage.
    pub max_prompt_tokens: u64,
    /// Provider-reported completion tokens, when the provider reports usage.
    pub max_completion_tokens: u64,
    /// Raw model response bytes. Always checkable, unlike token counts.
    pub max_response_bytes: u64,
    /// Wall-clock budget for one proposal turn including its repairs.
    pub max_turn_millis: u64,
    /// Repairs allowed after the first rejected response. `1` means one
    /// re-ask, i.e. at most two model responses in total.
    pub max_repairs: u32,
    /// Bytes the model may ask to type. Never above the run's own limit.
    pub max_text_entry_bytes: u32,
    /// Absolute scroll delta the model may request on either axis.
    pub max_scroll_delta: i32,
    /// Bytes of operator-facing proposal summary.
    pub max_summary_bytes: u32,
    /// When set, a proposal is refused outright unless the host supplies an
    /// independent verification of the observation binding. This is what
    /// makes the cheapest profile fail closed instead of trusting the model's
    /// own account of what it is looking at.
    pub requires_host_verification: bool,
}

impl ModelBoundaryCeilings {
    /// Proves a profile only narrows the provider-neutral safety kernel.
    ///
    /// A profile that tried to admit more text, more scroll travel, or more
    /// retries than the kernel's own hard ceiling would be an escalation
    /// dressed as a configuration value.
    pub fn validate(&self) -> Result<(), String> {
        let kernel = ComputerUseLimits::ceiling();
        if self.max_text_entry_bytes == 0 || self.max_text_entry_bytes > kernel.max_text_entry_bytes
        {
            return Err("profile text-entry ceiling escapes the kernel limit".into());
        }
        if self.max_scroll_delta <= 0 || self.max_scroll_delta > KERNEL_SCROLL_DELTA {
            return Err("profile scroll ceiling escapes the kernel limit".into());
        }
        if self.max_repairs > kernel.max_retries_per_action {
            return Err("profile repair budget escapes the kernel retry limit".into());
        }
        if self.max_observation_elements == 0
            || self.max_observation_elements > kernel.max_semantic_elements
        {
            return Err("profile element ceiling escapes the kernel limit".into());
        }
        if self.max_observation_bytes == 0 || self.max_observation_bytes > kernel.max_semantic_bytes
        {
            return Err("profile observation-byte ceiling escapes the kernel limit".into());
        }
        if self.max_element_text_bytes == 0 || self.max_element_text_bytes > KERNEL_LABEL_BYTES {
            return Err("profile element-text ceiling escapes the kernel limit".into());
        }
        if self.max_summary_bytes == 0 || self.max_summary_bytes > KERNEL_SUMMARY_BYTES {
            return Err("profile summary ceiling escapes the operator-facing limit".into());
        }
        if self.max_turn_millis == 0
            || self.max_turn_millis > kernel.max_duration_secs.saturating_mul(1_000)
        {
            return Err("profile turn budget escapes the run duration limit".into());
        }
        if self.max_prompt_tokens == 0
            || self.max_completion_tokens == 0
            || self.max_response_bytes == 0
        {
            return Err("profile token or response ceiling is zero".into());
        }
        Ok(())
    }
}

/// Mirrors the per-action scroll bound enforced by `ComputerAction::validate`.
const KERNEL_SCROLL_DELTA: i32 = 10_000;
/// Mirrors `computer_use::MAX_LABEL_BYTES`, the kernel's label/value bound.
const KERNEL_LABEL_BYTES: u32 = 512;
/// Operator-facing proposal summaries are bounded by the wire schema at 512.
const KERNEL_SUMMARY_BYTES: u32 = 512;

/// Model class a Computer proposal is being taken from.
///
/// Ordering is authority order, so `profile >= Balanced` reads correctly.
/// [`Self::Efficient`] is the [`Default`] because an unattributed model must
/// land on the strictest contract, never the most generous one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ModelBoundaryProfile {
    /// Small/cheap or locally qualified models. Semantic-only context, one
    /// repair, and no proposal at all without host verification.
    #[default]
    Efficient,
    /// Durably qualified semantic models. Geometry, two repairs.
    Balanced,
    /// Durably qualified visual-fallback models. Redacted evidence reference.
    Frontier,
}

impl ModelBoundaryProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Efficient => "efficient",
            Self::Balanced => "balanced",
            Self::Frontier => "frontier",
        }
    }

    /// The profile's ceilings. A `const` table, not a computed policy.
    pub fn ceilings(self) -> ModelBoundaryCeilings {
        match self {
            Self::Efficient => ModelBoundaryCeilings {
                observation_detail: ObservationDetail::SemanticOnly,
                max_observation_elements: 48,
                max_observation_bytes: 24 * 1024,
                max_element_text_bytes: 256,
                max_prompt_tokens: 8_000,
                max_completion_tokens: 512,
                max_response_bytes: 8 * 1024,
                max_turn_millis: 20_000,
                max_repairs: 1,
                max_text_entry_bytes: 1_024,
                max_scroll_delta: 2_000,
                max_summary_bytes: 256,
                requires_host_verification: true,
            },
            Self::Balanced => ModelBoundaryCeilings {
                observation_detail: ObservationDetail::SemanticWithGeometry,
                max_observation_elements: 256,
                max_observation_bytes: 128 * 1024,
                max_element_text_bytes: 512,
                max_prompt_tokens: 32_000,
                max_completion_tokens: 2_048,
                max_response_bytes: 32 * 1024,
                max_turn_millis: 60_000,
                max_repairs: 2,
                max_text_entry_bytes: 4 * 1024,
                max_scroll_delta: 10_000,
                max_summary_bytes: 512,
                requires_host_verification: false,
            },
            Self::Frontier => ModelBoundaryCeilings {
                observation_detail: ObservationDetail::SemanticWithEvidenceRef,
                max_observation_elements: 1_024,
                max_observation_bytes: 512 * 1024,
                max_element_text_bytes: 512,
                max_prompt_tokens: 128_000,
                max_completion_tokens: 4_096,
                max_response_bytes: 128 * 1024,
                max_turn_millis: 120_000,
                max_repairs: 3,
                max_text_entry_bytes: ComputerUseLimits::ceiling().max_text_entry_bytes,
                max_scroll_delta: 10_000,
                max_summary_bytes: 512,
                requires_host_verification: false,
            },
        }
    }

    /// Profile for a model whose Computer authority is durably attributed to
    /// its provider profile.
    ///
    /// Tiers below `SemanticAct` never reach a proposal, but they map to the
    /// strictest profile anyway so a future caller cannot acquire a generous
    /// budget by arriving with a weaker tier.
    pub fn for_tier(tier: ComputerUseTier) -> Self {
        match tier {
            ComputerUseTier::VisualFallbackAct => Self::Frontier,
            ComputerUseTier::SemanticAct => Self::Balanced,
            ComputerUseTier::Observe | ComputerUseTier::None => Self::Efficient,
        }
    }

    /// Profile for the authority actually in force at the call site.
    ///
    /// `durable_authority` is a capability recorded against the provider
    /// profile. Without it the only thing a model has proved is that it can
    /// satisfy this process's deterministic simulator qualification, which is
    /// exactly the small-local-model case: hold it to `Efficient`.
    pub fn for_authority(tier: ComputerUseTier, durable_authority: bool) -> Self {
        if durable_authority {
            Self::for_tier(tier)
        } else {
            Self::Efficient
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ModelBoundaryProfile; 3] = [
        ModelBoundaryProfile::Efficient,
        ModelBoundaryProfile::Balanced,
        ModelBoundaryProfile::Frontier,
    ];

    #[test]
    fn every_profile_only_narrows_the_safety_kernel() {
        for profile in ALL {
            profile
                .ceilings()
                .validate()
                .unwrap_or_else(|error| panic!("{} ceilings: {error}", profile.as_str()));
        }
    }

    #[test]
    fn efficient_is_the_default_and_the_strictest_profile() {
        assert_eq!(
            ModelBoundaryProfile::default(),
            ModelBoundaryProfile::Efficient
        );
        let efficient = ModelBoundaryProfile::Efficient.ceilings();
        for profile in ALL {
            let other = profile.ceilings();
            assert!(efficient.max_observation_elements <= other.max_observation_elements);
            assert!(efficient.max_observation_bytes <= other.max_observation_bytes);
            assert!(efficient.max_prompt_tokens <= other.max_prompt_tokens);
            assert!(efficient.max_completion_tokens <= other.max_completion_tokens);
            assert!(efficient.max_response_bytes <= other.max_response_bytes);
            assert!(efficient.max_turn_millis <= other.max_turn_millis);
            assert!(efficient.max_repairs <= other.max_repairs);
            assert!(efficient.max_text_entry_bytes <= other.max_text_entry_bytes);
            assert!(efficient.max_scroll_delta <= other.max_scroll_delta);
        }
    }

    #[test]
    fn efficient_is_semantic_only_with_one_repair_and_required_verification() {
        let ceilings = ModelBoundaryProfile::Efficient.ceilings();
        assert_eq!(ceilings.observation_detail, ObservationDetail::SemanticOnly);
        assert!(!ceilings.observation_detail.allows_geometry());
        assert!(!ceilings.observation_detail.allows_evidence_reference());
        assert!(!ceilings.observation_detail.allows_screenshot_bytes());
        assert_eq!(ceilings.max_repairs, 1);
        assert!(ceilings.requires_host_verification);
    }

    #[test]
    fn no_profile_ever_admits_screenshot_bytes() {
        for profile in ALL {
            assert!(!profile
                .ceilings()
                .observation_detail
                .allows_screenshot_bytes());
        }
    }

    #[test]
    fn authority_mapping_falls_back_to_efficient_without_durable_capability() {
        assert_eq!(
            ModelBoundaryProfile::for_authority(ComputerUseTier::VisualFallbackAct, true),
            ModelBoundaryProfile::Frontier
        );
        assert_eq!(
            ModelBoundaryProfile::for_authority(ComputerUseTier::SemanticAct, true),
            ModelBoundaryProfile::Balanced
        );
        assert_eq!(
            ModelBoundaryProfile::for_authority(ComputerUseTier::VisualFallbackAct, false),
            ModelBoundaryProfile::Efficient
        );
        assert_eq!(
            ModelBoundaryProfile::for_tier(ComputerUseTier::Observe),
            ModelBoundaryProfile::Efficient
        );
        assert_eq!(
            ModelBoundaryProfile::for_tier(ComputerUseTier::None),
            ModelBoundaryProfile::Efficient
        );
    }

    #[test]
    fn ceiling_validation_rejects_a_widened_profile() {
        let mut widened = ModelBoundaryProfile::Balanced.ceilings();
        widened.max_scroll_delta = KERNEL_SCROLL_DELTA + 1;
        assert!(widened.validate().is_err());

        let mut widened = ModelBoundaryProfile::Balanced.ceilings();
        widened.max_text_entry_bytes = ComputerUseLimits::ceiling().max_text_entry_bytes + 1;
        assert!(widened.validate().is_err());

        let mut widened = ModelBoundaryProfile::Balanced.ceilings();
        widened.max_repairs = ComputerUseLimits::ceiling().max_retries_per_action + 1;
        assert!(widened.validate().is_err());
    }
}
