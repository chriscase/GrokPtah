//! One fail-closed authority for provider capability generations (#458).
//!
//! # The hole this closes
//!
//! Computer Use session qualification used to be bound to a *route
//! fingerprint*: base URL, wire model, dialect. That fingerprint is stable
//! across exactly the changes that matter. A provider can keep serving the
//! same endpoint and the same wire model while the capability record behind
//! it is rewritten — a measured tier replaced by a declared one, a schema
//! bumped, a credential rotated to a different principal, an operator policy
//! narrowed, a requalification failing outright. None of that moves the route
//! fingerprint, so a session that was qualified once kept model-action
//! authority it no longer had.
//!
//! This module replaces that fingerprint with a *capability generation*: a
//! secret-free binding of everything that must still be true for a
//! qualification to still mean what it meant when it was taken.
//!
//! # What is bound
//!
//! [`CapabilityFacts`] carries the whole binding, and [`CapabilityDigest`] is
//! its content address:
//!
//! - normalized route identity (provider id, normalized base URL, wire model,
//!   dialect) — see [`NormalizedRoute`];
//! - the effective Computer Use tier *and* the assurance profile in force;
//! - capability provenance, including whether a `Declared` statement was
//!   trusted and under which operator-configured provenance;
//! - the qualification schema id and version, so schema drift invalidates;
//! - a secret-free credential incarnation: the principal fingerprint plus a
//!   monotonic incarnation that advances on rotation *and* on deletion, so a
//!   credential that is removed and re-added never lands on its old identity;
//! - the policy/allowlist revision;
//! - the measured or signed qualification evidence digest.
//!
//! Two halves guard different failures, and both are checked:
//!
//! - the [`CapabilityGeneration`] stamp (an authority id plus a monotonic
//!   counter) catches events that revoke authority without changing any
//!   observable fact — an explicit revocation, a failed requalification, a
//!   process restart;
//! - the [`CapabilityDigest`] catches drift in the facts themselves, even if
//!   an advance were somehow missed.
//!
//! # Boundaries
//!
//! A binding is minted once, at qualification, and is re-validated at every
//! [`CapabilityBoundary`]: observation, model proposal, staging, approval,
//! lease acquisition, live-frame delivery, and dispatch. Dispatch and
//! live-frame delivery re-derive the facts from live state *at the moment of
//! the call*, so the check sits immediately before the physical action and on
//! every frame rather than at the start of a long-lived operation.
//!
//! # Fail-closed vocabulary
//!
//! Every refusal is the same [`CapabilityDenied`] value with the same message.
//! A foreign binding, an unknown binding, a revoked binding and a stale
//! binding are indistinguishable to the caller, so a denial cannot be used to
//! probe which bindings exist or why one stopped working.
//!
//! # Not in scope
//!
//! This module mints no principals (#477), owns no runtime lifecycle
//! (#455/#468), owns no queue (#461), sends nothing to a provider (#478), and
//! writes no audit journal (#462). It answers exactly one question: is this
//! capability still the capability that was qualified?

mod boundary;
mod digest;
mod generation;
mod policy;
mod profile;
mod registry;

pub use boundary::{BoundarySet, CapabilityBoundary};
pub use digest::{
    normalize_base_url, CapabilityDigest, CapabilityFacts, CredentialIncarnation, NormalizedRoute,
    PolicyRevision, QualificationEvidence, QualificationEvidenceKind, QualificationSchema,
};
pub use generation::{CapabilityDenied, CapabilityGeneration};
pub use policy::{CapabilityProvenance, DeclaredCapabilityPolicy};
pub use profile::{AssuranceCeilings, AssuranceProfile};
pub use registry::{
    CapabilityAssessment, CapabilityBindingRef, CapabilityRegistry, CapabilityRequest,
    QualificationKey,
};

/// Operator setting naming the assurance profile Computer Use runs under.
pub const ASSURANCE_PROFILE_ENV: &str = "GROKPTAH_COMPUTER_ASSURANCE_PROFILE";

/// Operator setting naming a trusted provenance for `Declared` capability.
///
/// Unset means declaration is never durable action authority. Setting it is
/// the explicit decision the deployment has to make to change that, and the
/// name given here is published in every binding it authorizes.
pub const DECLARED_TRUST_ENV: &str = "GROKPTAH_COMPUTER_DECLARED_TRUST_PROVENANCE";

/// The assurance profile a deployment gets when it has not chosen one.
pub const DEFAULT_ASSURANCE_PROFILE: AssuranceProfile = AssuranceProfile::Balanced;

/// Resolves operator capability policy from two configuration values.
///
/// A pure function so the policy a deployment ends up with can be tested
/// without touching process environment. Both fallbacks narrow:
///
/// - an unset profile lands on [`DEFAULT_ASSURANCE_PROFILE`];
/// - an *unrecognised* profile lands on [`AssuranceProfile::HighAssurance`],
///   because a misspelled setting must not quietly leave the deployment on
///   something broader than the operator meant;
/// - an unset, blank, or unusable declared-trust name lands on
///   [`DeclaredCapabilityPolicy::ObservationOnly`].
pub fn operator_policy(
    assurance_profile: Option<&str>,
    declared_trust: Option<&str>,
) -> (AssuranceProfile, DeclaredCapabilityPolicy) {
    let profile = match assurance_profile.map(str::trim).filter(|v| !v.is_empty()) {
        None => DEFAULT_ASSURANCE_PROFILE,
        Some(value) => AssuranceProfile::parse(value).unwrap_or(AssuranceProfile::HighAssurance),
    };
    let declared = declared_trust
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| DeclaredCapabilityPolicy::trusted(value).ok())
        .unwrap_or_default();
    (profile, declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_deployment_never_trusts_declaration() {
        let (profile, declared) = operator_policy(None, None);
        assert_eq!(profile, DEFAULT_ASSURANCE_PROFILE);
        assert_eq!(declared, DeclaredCapabilityPolicy::ObservationOnly);
        assert!(!declared.trusts_declaration());
    }

    #[test]
    fn a_misspelled_profile_narrows_instead_of_widening() {
        for spelling in ["fronteir", "ECONOMY", "efficient", "  "] {
            let (profile, _) = operator_policy(Some(spelling), None);
            let expected = if spelling.trim().is_empty() {
                DEFAULT_ASSURANCE_PROFILE
            } else {
                AssuranceProfile::HighAssurance
            };
            assert_eq!(profile, expected, "{spelling:?}");
        }
        for profile in AssuranceProfile::ALL {
            assert_eq!(operator_policy(Some(profile.as_str()), None).0, profile);
        }
    }

    #[test]
    fn declared_trust_takes_a_usable_name_or_none_at_all() {
        let (_, trusted) = operator_policy(None, Some("operator-manifest"));
        assert!(trusted.trusts_declaration());
        for unusable in ["", "   ", "bad\nname"] {
            let (_, declared) = operator_policy(None, Some(unusable));
            assert_eq!(
                declared,
                DeclaredCapabilityPolicy::ObservationOnly,
                "{unusable:?}"
            );
        }
    }
}
