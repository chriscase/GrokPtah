//! Operator policy for `Declared` capability, and the provenance it produces.
//!
//! A `Measured` capability statement is the record of a probe that actually
//! ran. A `Declared` one is a statement someone wrote down. Treating the two
//! alike is how a configuration edit becomes durable authority to drive a
//! screen, so this module keeps them apart and makes the operator say, in
//! configuration, which one they meant.

use serde::{Deserialize, Serialize};

use super::profile::AssuranceProfile;
use crate::gateway_config::{CapabilitySource, ComputerUseTier};

/// Longest operator-supplied provenance label accepted.
const MAX_PROVENANCE_ID_BYTES: usize = 128;

/// What the deployment does with a `Declared` capability statement.
///
/// The default is [`Self::ObservationOnly`]: an unconfigured deployment treats
/// declaration as metadata. Trusting declaration is possible, but only as an
/// explicit operator decision that names where the declaration comes from, and
/// that name is bound into the capability digest — so revoking the trust, or
/// changing where declarations come from, invalidates every binding taken
/// under it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DeclaredCapabilityPolicy {
    /// Declared capability qualifies observation and nothing more. It can
    /// never become durable action authority, whatever tier the record claims.
    #[default]
    ObservationOnly,
    /// The operator has explicitly configured a trusted provenance for
    /// declared capability on this deployment. `provenance_id` names it and is
    /// published in the binding, so an operator reading a qualification can
    /// see that its authority rests on a declaration and on which one.
    #[serde(rename_all = "camelCase")]
    TrustedProvenance { provenance_id: String },
}

impl DeclaredCapabilityPolicy {
    /// Builds a trusted-provenance policy, rejecting a label that could not be
    /// shown to an operator as an unambiguous source name.
    pub fn trusted(provenance_id: impl Into<String>) -> Result<Self, String> {
        let provenance_id = provenance_id.into();
        let trimmed = provenance_id.trim();
        if trimmed.is_empty() {
            return Err("declared-capability trust requires a named provenance".into());
        }
        if trimmed.len() > MAX_PROVENANCE_ID_BYTES {
            return Err("declared-capability provenance name is too long".into());
        }
        if trimmed
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() && c != ' ')
        {
            return Err("declared-capability provenance name has control characters".into());
        }
        Ok(Self::TrustedProvenance {
            provenance_id: trimmed.to_string(),
        })
    }

    /// Whether this policy admits declared capability as action authority at
    /// all. Answering `false` is the default.
    pub fn trusts_declaration(&self) -> bool {
        matches!(self, Self::TrustedProvenance { .. })
    }
}

/// How a capability's authority was established, after policy is applied.
///
/// This is the operator-facing answer to "why does this model get to act?",
/// and it is bound into the digest, so it cannot change under a binding
/// without invalidating it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapabilityProvenance {
    /// Nothing established it. No authority.
    Unknown,
    /// A bounded local probe established it.
    Measured,
    /// A probe whose transcript carries an operator-verified signature.
    Signed,
    /// Declared, and the operator has explicitly trusted this provenance.
    #[serde(rename_all = "camelCase")]
    DeclaredTrusted { provenance_id: String },
    /// Declared, and the deployment does not trust declaration for action.
    /// Observation only.
    DeclaredObservationOnly,
}

impl CapabilityProvenance {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Measured => "measured",
            Self::Signed => "signed",
            Self::DeclaredTrusted { .. } => "declared_trusted",
            Self::DeclaredObservationOnly => "declared_observation_only",
        }
    }

    pub fn provenance_id(&self) -> Option<&str> {
        match self {
            Self::DeclaredTrusted { provenance_id } => Some(provenance_id),
            _ => None,
        }
    }

    /// Whether this provenance may carry durable action authority. Declared
    /// provenance says `true` only when the operator configured trust *and*
    /// the assurance profile honours it.
    pub fn admits_action(&self) -> bool {
        matches!(
            self,
            Self::Measured | Self::Signed | Self::DeclaredTrusted { .. }
        )
    }
}

/// Applies operator policy to a raw capability record.
///
/// Returns the tier that may actually be exercised and the provenance that
/// explains it. The tier is only ever narrowed here: this function has no path
/// that returns more authority than the record already claimed, and no path
/// that falls back to a broader provider profile when the exact model record
/// is missing.
pub(super) fn resolve_provenance(
    source: CapabilitySource,
    claimed_tier: ComputerUseTier,
    profile: AssuranceProfile,
    declared_policy: &DeclaredCapabilityPolicy,
) -> (ComputerUseTier, CapabilityProvenance) {
    let (mut tier, provenance) = match source {
        CapabilitySource::Unknown => (ComputerUseTier::None, CapabilityProvenance::Unknown),
        CapabilitySource::Measured => (claimed_tier, CapabilityProvenance::Measured),
        CapabilitySource::Declared => match declared_policy {
            // A declaration the deployment does not trust cannot exceed
            // observation, whatever tier the record claims.
            DeclaredCapabilityPolicy::ObservationOnly => (
                claimed_tier.min(ComputerUseTier::Observe),
                CapabilityProvenance::DeclaredObservationOnly,
            ),
            DeclaredCapabilityPolicy::TrustedProvenance { provenance_id } => {
                if profile.honours_declared_trust() {
                    (
                        claimed_tier,
                        CapabilityProvenance::DeclaredTrusted {
                            provenance_id: provenance_id.clone(),
                        },
                    )
                } else {
                    // The profile outranks the trust setting. A profile that
                    // does not honour declaration narrows to observation
                    // rather than silently accepting the operator's trust.
                    (
                        claimed_tier.min(ComputerUseTier::Observe),
                        CapabilityProvenance::DeclaredObservationOnly,
                    )
                }
            }
        },
    };
    if tier > ComputerUseTier::Observe && !provenance.admits_action() {
        tier = ComputerUseTier::Observe;
    }
    (tier, provenance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_is_observation_only_by_default() {
        let policy = DeclaredCapabilityPolicy::default();
        assert!(!policy.trusts_declaration());
        for profile in AssuranceProfile::ALL {
            let (tier, provenance) = resolve_provenance(
                CapabilitySource::Declared,
                ComputerUseTier::VisualFallbackAct,
                profile,
                &policy,
            );
            assert_eq!(tier, ComputerUseTier::Observe, "{profile:?}");
            assert_eq!(provenance, CapabilityProvenance::DeclaredObservationOnly);
            assert!(!provenance.admits_action());
        }
    }

    #[test]
    fn declared_action_authority_requires_explicit_named_trust() {
        assert!(DeclaredCapabilityPolicy::trusted("   ").is_err());
        assert!(DeclaredCapabilityPolicy::trusted("a\nb").is_err());
        assert!(DeclaredCapabilityPolicy::trusted("x".repeat(200)).is_err());
        let policy = DeclaredCapabilityPolicy::trusted("operator-manifest").expect("policy");
        let (tier, provenance) = resolve_provenance(
            CapabilitySource::Declared,
            ComputerUseTier::SemanticAct,
            AssuranceProfile::Balanced,
            &policy,
        );
        assert_eq!(tier, ComputerUseTier::SemanticAct);
        assert_eq!(provenance.provenance_id(), Some("operator-manifest"));
        assert!(provenance.admits_action());
    }

    #[test]
    fn high_assurance_refuses_declared_trust_even_when_configured() {
        let policy = DeclaredCapabilityPolicy::trusted("operator-manifest").expect("policy");
        let (tier, provenance) = resolve_provenance(
            CapabilitySource::Declared,
            ComputerUseTier::SemanticAct,
            AssuranceProfile::HighAssurance,
            &policy,
        );
        assert_eq!(tier, ComputerUseTier::Observe);
        assert_eq!(provenance, CapabilityProvenance::DeclaredObservationOnly);
    }

    #[test]
    fn unknown_source_is_never_authority_and_policy_only_narrows() {
        let trusted = DeclaredCapabilityPolicy::trusted("operator-manifest").expect("policy");
        let (tier, provenance) = resolve_provenance(
            CapabilitySource::Unknown,
            ComputerUseTier::VisualFallbackAct,
            AssuranceProfile::Economy,
            &trusted,
        );
        assert_eq!(tier, ComputerUseTier::None);
        assert_eq!(provenance, CapabilityProvenance::Unknown);
        for source in [
            CapabilitySource::Unknown,
            CapabilitySource::Declared,
            CapabilitySource::Measured,
        ] {
            for profile in AssuranceProfile::ALL {
                for policy in [DeclaredCapabilityPolicy::default(), trusted.clone()] {
                    let claimed = ComputerUseTier::SemanticAct;
                    let (tier, _) = resolve_provenance(source, claimed, profile, &policy);
                    assert!(tier <= claimed, "{source:?}/{profile:?} widened authority");
                }
            }
        }
    }
}
