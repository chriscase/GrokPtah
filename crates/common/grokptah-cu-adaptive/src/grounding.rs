//! Visual and semantic grounding requirements.
//!
//! Grounding is the evidence that the thing a step names is the thing that is
//! actually there. It is the difference between "click the Save button" and
//! "click at (412, 288)": the first can be checked against the live frame, the
//! second can only be checked against a memory of one.
//!
//! Three levels, ordered:
//!
//! * [`GroundingLevel::None`] -- nothing to check. Only ever enough for steps
//!   that name no element at all (waiting, observing, activating the already
//!   authorized target).
//! * [`GroundingLevel::Semantic`] -- the step names an element, and the
//!   element's identity and role digest match the live frame.
//! * [`GroundingLevel::SemanticPlusRegion`] -- as above, plus a region digest
//!   that pins what was actually rendered where the element is.
//!
//! Two rules do most of the work here:
//!
//! 1. **A pointer step always needs region grounding, and a class that cannot
//!    localize may never take one.** A pixel-blind model proposing a click is
//!    guessing coordinates from a semantic tree; the refusal is
//!    [`DenyReason::PointerWithoutVisualGrounding`], and it is not a threshold
//!    a profile can lower.
//! 2. **A claim is verified against the live frame, never against the frame it
//!    was made on.** A claim that matched at plan time and does not match now
//!    is [`DenyReason::TargetMissing`] or [`DenyReason::TargetDrifted`], which
//!    is the whole reason the executor re-derives instead of trusting.
//!
//! Region grounding carries a *digest* of the rendered region, never the
//! pixels and never a path to them. The benchmark can tell that a region
//! changed; it cannot tell what was in it, and neither can a receipt.

use serde::{Deserialize, Serialize};

use crate::digest::is_digest;
use crate::profile::ExecutionProfile;
use crate::redaction::Sensitivity;
use crate::schema::{ElementRef, IntentFamily};
use crate::tier::DeclaredTierCapability;
use crate::vocabulary::DenyReason;

/// How much evidence backs a step's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingLevel {
    None,
    Semantic,
    SemanticPlusRegion,
}

impl GroundingLevel {
    pub const ALL: &'static [GroundingLevel] =
        &[Self::None, Self::Semantic, Self::SemanticPlusRegion];
}

/// The evidence a step actually carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroundingClaim {
    /// The step names no element.
    None,
    /// The step names an element and its role.
    Semantic {
        element: ElementRef,
        role_digest: String,
    },
    /// The step names an element, its role, and the region that rendered it.
    SemanticPlusRegion {
        element: ElementRef,
        role_digest: String,
        region_digest: String,
    },
}

impl GroundingClaim {
    #[must_use]
    pub fn level(&self) -> GroundingLevel {
        match self {
            Self::None => GroundingLevel::None,
            Self::Semantic { .. } => GroundingLevel::Semantic,
            Self::SemanticPlusRegion { .. } => GroundingLevel::SemanticPlusRegion,
        }
    }

    #[must_use]
    pub fn element(&self) -> Option<&ElementRef> {
        match self {
            Self::None => None,
            Self::Semantic { element, .. } | Self::SemanticPlusRegion { element, .. } => {
                Some(element)
            }
        }
    }

    #[must_use]
    pub fn role_digest(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Semantic { role_digest, .. } | Self::SemanticPlusRegion { role_digest, .. } => {
                Some(role_digest)
            }
        }
    }

    #[must_use]
    pub fn region_digest(&self) -> Option<&str> {
        match self {
            Self::SemanticPlusRegion { region_digest, .. } => Some(region_digest),
            _ => None,
        }
    }

    /// True when every digest present is a well-formed digest and every
    /// element reference is well-formed. A malformed claim is a schema
    /// violation, not weak grounding.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let digests_ok =
            self.role_digest().is_none_or(is_digest) && self.region_digest().is_none_or(is_digest);
        digests_ok && self.element().is_none_or(ElementRef::is_well_formed)
    }
}

/// What the live frame says about the element a claim names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveElement {
    pub element: ElementRef,
    pub role_digest: String,
    pub region_digest: String,
    pub enabled: bool,
    pub sensitivity: Sensitivity,
    pub advertises: bool,
}

/// The grounding level a step must reach.
///
/// Profiles may only ever *raise* this. [`ExecutionProfile::grounding_floor`]
/// is added on top of the intent's own requirement, so a cheap profile lands
/// on the intent's floor and an expensive one lands above it. There is no
/// arithmetic here that can produce a level below what the intent demands.
#[must_use]
pub fn required_level(profile: &ExecutionProfile, family: IntentFamily) -> GroundingLevel {
    let intrinsic = match family {
        // Nothing is named, so there is nothing to ground.
        IntentFamily::Ambient => GroundingLevel::None,
        // Names an element; its identity and role must match the live frame.
        IntentFamily::Semantic | IntentFamily::TextEntry => GroundingLevel::Semantic,
        // Reaches application-global commands without naming a target.
        IntentFamily::KeyChord => GroundingLevel::None,
        // Leaves the semantic surface entirely.
        IntentFamily::PointerFallback => GroundingLevel::SemanticPlusRegion,
    };
    intrinsic.max(profile.grounding_floor_for(family))
}

/// Verify a claim against the live frame.
///
/// The order matters and is deliberate: hard-denied surfaces are refused
/// before capability, capability before sufficiency, sufficiency before
/// freshness. A caller that reads only the first refusal still reads the most
/// important one.
pub fn verify(
    profile: &ExecutionProfile,
    tier: &DeclaredTierCapability,
    family: IntentFamily,
    claim: &GroundingClaim,
    live: Option<&LiveElement>,
) -> Result<(), DenyReason> {
    if !claim.is_well_formed() {
        return Err(DenyReason::SchemaViolation);
    }

    // A pointer step from a class that cannot localize is refused before any
    // question about evidence, because no amount of evidence fixes it.
    if family == IntentFamily::PointerFallback && tier.pixel_blind() {
        return Err(DenyReason::PointerWithoutVisualGrounding);
    }

    let required = required_level(profile, family);
    if claim.level() < required {
        return if family == IntentFamily::PointerFallback {
            Err(DenyReason::PointerWithoutVisualGrounding)
        } else {
            Err(DenyReason::GroundingInsufficient)
        };
    }

    let Some(claimed_element) = claim.element() else {
        // Nothing named: the requirement was None, and we have already checked
        // the claim reaches it.
        return Ok(());
    };

    let Some(live) = live else {
        return Err(DenyReason::TargetMissing);
    };
    if live.sensitivity.is_hard_denied() {
        return Err(DenyReason::SensitiveSurface);
    }
    if live.element.element_id != claimed_element.element_id {
        return Err(DenyReason::TargetMissing);
    }
    if live.element.generation != claimed_element.generation {
        return Err(DenyReason::TargetDrifted);
    }
    if claim.role_digest() != Some(live.role_digest.as_str()) {
        // Same identity, different role: the application reused the id for a
        // different control. Treating that as drift rather than as a match is
        // the difference between clicking Save and clicking Delete.
        return Err(DenyReason::TargetDrifted);
    }
    if !live.enabled {
        return Err(DenyReason::ElementDisabled);
    }
    if !live.advertises {
        return Err(DenyReason::ActionNotAdvertised);
    }
    if claim
        .region_digest()
        .is_some_and(|claimed| claimed != live.region_digest)
    {
        return Err(DenyReason::StaleFrame);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::{digest_str, domain};
    use crate::profile::ProfileId;
    use crate::tier::ModelTier;

    fn element() -> ElementRef {
        ElementRef::new("save-button", 3).unwrap()
    }

    fn role() -> String {
        digest_str(domain::ELEMENT_ROLE, "button")
    }

    fn region() -> String {
        digest_str(domain::REGION, "region-bytes")
    }

    fn live() -> LiveElement {
        LiveElement {
            element: element(),
            role_digest: role(),
            region_digest: region(),
            enabled: true,
            sensitivity: Sensitivity::None,
            advertises: true,
        }
    }

    fn semantic_claim() -> GroundingClaim {
        GroundingClaim::Semantic {
            element: element(),
            role_digest: role(),
        }
    }

    fn pointer_claim() -> GroundingClaim {
        GroundingClaim::SemanticPlusRegion {
            element: element(),
            role_digest: role(),
            region_digest: region(),
        }
    }

    #[test]
    fn a_pixel_blind_class_can_never_take_a_pointer_step() {
        let profile = ProfileId::HighAssurance.spec();
        for tier in [ModelTier::SmallLocal, ModelTier::MidVision] {
            let err = verify(
                &profile,
                &tier.declared(),
                IntentFamily::PointerFallback,
                &pointer_claim(),
                Some(&live()),
            )
            .unwrap_err();
            assert_eq!(err, DenyReason::PointerWithoutVisualGrounding);
        }
        assert!(
            verify(
                &profile,
                &ModelTier::StrongHosted.declared(),
                IntentFamily::PointerFallback,
                &pointer_claim(),
                Some(&live()),
            )
            .is_ok()
        );
    }

    #[test]
    fn pointer_steps_are_refused_without_region_grounding_even_when_capable() {
        let err = verify(
            &ProfileId::Economy.spec(),
            &ModelTier::StrongHosted.declared(),
            IntentFamily::PointerFallback,
            &semantic_claim(),
            Some(&live()),
        )
        .unwrap_err();
        assert_eq!(err, DenyReason::PointerWithoutVisualGrounding);
    }

    #[test]
    fn recycled_element_ids_read_as_drift_not_as_a_match() {
        let mut live = live();
        live.role_digest = digest_str(domain::ELEMENT_ROLE, "menu_item");
        let err = verify(
            &ProfileId::Balanced.spec(),
            &ModelTier::StrongHosted.declared(),
            IntentFamily::Semantic,
            &semantic_claim(),
            Some(&live),
        )
        .unwrap_err();
        assert_eq!(err, DenyReason::TargetDrifted);
    }

    #[test]
    fn a_stale_region_digest_refuses_the_step() {
        let mut live = live();
        live.region_digest = digest_str(domain::REGION, "different-bytes");
        let err = verify(
            &ProfileId::Balanced.spec(),
            &ModelTier::StrongHosted.declared(),
            IntentFamily::PointerFallback,
            &pointer_claim(),
            Some(&live),
        )
        .unwrap_err();
        assert_eq!(err, DenyReason::StaleFrame);
    }

    #[test]
    fn hard_denial_outranks_every_other_refusal() {
        let mut live = live();
        live.sensitivity = Sensitivity::Secure;
        live.enabled = false;
        live.advertises = false;
        let err = verify(
            &ProfileId::Economy.spec(),
            &ModelTier::StrongHosted.declared(),
            IntentFamily::Semantic,
            &semantic_claim(),
            Some(&live),
        )
        .unwrap_err();
        assert_eq!(err, DenyReason::SensitiveSurface);
    }

    #[test]
    fn profiles_only_raise_the_requirement() {
        for family in [
            IntentFamily::Ambient,
            IntentFamily::Semantic,
            IntentFamily::TextEntry,
            IntentFamily::KeyChord,
            IntentFamily::PointerFallback,
        ] {
            let economy = required_level(&ProfileId::Economy.spec(), family);
            let balanced = required_level(&ProfileId::Balanced.spec(), family);
            let assured = required_level(&ProfileId::HighAssurance.spec(), family);
            assert!(balanced >= economy);
            assert!(assured >= balanced);
        }
    }

    #[test]
    fn malformed_digests_are_schema_violations() {
        let claim = GroundingClaim::Semantic {
            element: element(),
            role_digest: "not-a-digest".into(),
        };
        let err = verify(
            &ProfileId::Balanced.spec(),
            &ModelTier::StrongHosted.declared(),
            IntentFamily::Semantic,
            &claim,
            Some(&live()),
        )
        .unwrap_err();
        assert_eq!(err, DenyReason::SchemaViolation);
    }
}
