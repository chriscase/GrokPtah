//! Assurance profiles: one deterministic ceiling set per deployment posture.
//!
//! The three profiles exist so a deployment running a small local model and a
//! deployment running a frontier model can both use this authority without
//! either one bending the other's contract. They do **not** differ in what is
//! checked. Every profile validates the same eight boundaries, produces the
//! same indistinguishable refusal, and binds the same facts into the same
//! digest. What differs is how long a qualification is allowed to stand and
//! what class of evidence a deployment insists on before an action is
//! attributed to a model.
//!
//! Every value here is a constant. Nothing is derived from provider metadata,
//! model self-report, or an observation, so two runs of the same profile admit
//! exactly the same things. [`AssuranceCeilings::validate`] proves each
//! profile narrows a fixed kernel ceiling rather than widening it, and the
//! tests below run it for every profile.

use serde::{Deserialize, Serialize};

use super::digest::QualificationEvidenceKind;

/// Longest a qualification may stand under any profile.
///
/// Mirrors `ComputerUseLimits::default().max_duration_secs`: a qualification
/// must not outlive the run duration the provider-neutral kernel admits by
/// default, or a run could be driven end to end on evidence taken before it
/// started.
const KERNEL_MAX_QUALIFICATION_AGE_SECS: u64 = 15 * 60;

/// Most dispatches one qualification may authorize under any profile.
///
/// Mirrors `ComputerUseLimits::default().max_actions`, so a qualification can
/// never authorize more physical actions than a default run is allowed to
/// take.
const KERNEL_MAX_DISPATCHES_PER_QUALIFICATION: u32 = 64;

/// Deterministic per-profile ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssuranceCeilings {
    /// Wall-clock life of one qualification before requalification is
    /// required. Measured on a monotonic clock, so a system-clock change
    /// cannot extend it.
    pub max_qualification_age_secs: u64,
    /// Physical dispatches one qualification may authorize.
    pub max_dispatches_per_qualification: u32,
    /// Weakest evidence class this profile accepts as durable action
    /// authority.
    pub minimum_action_evidence: QualificationEvidenceKind,
    /// Whether an operator-configured declared-capability trust is honoured at
    /// all. A profile that answers `false` narrows declared capability to
    /// observation regardless of configuration.
    pub honours_declared_trust: bool,
}

impl AssuranceCeilings {
    /// Proves this profile only narrows the kernel.
    ///
    /// A profile that admitted a longer-lived qualification or more dispatches
    /// than the kernel allows would be an escalation dressed as a
    /// configuration value.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_qualification_age_secs == 0
            || self.max_qualification_age_secs > KERNEL_MAX_QUALIFICATION_AGE_SECS
        {
            return Err("profile qualification lifetime escapes the kernel limit".into());
        }
        if self.max_dispatches_per_qualification == 0
            || self.max_dispatches_per_qualification > KERNEL_MAX_DISPATCHES_PER_QUALIFICATION
        {
            return Err("profile dispatch budget escapes the kernel limit".into());
        }
        if self.minimum_action_evidence < QualificationEvidenceKind::Measured {
            return Err("profile would accept action authority without measured evidence".into());
        }
        Ok(())
    }
}

/// Deployment posture a capability binding is taken under.
///
/// There is deliberately no `Default`. A profile is an operator decision, and
/// a silent default is exactly the "fell back to something broader" failure
/// this authority exists to prevent — the registry takes it explicitly at
/// construction, and changing it invalidates every binding taken under the old
/// one.
///
/// Ordering is assurance order, so `profile >= Balanced` reads correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceProfile {
    /// Small or cheap models. Short qualification life and a small dispatch
    /// budget, because a cheap model's behaviour is the thing most likely to
    /// drift between one probe and the next.
    Economy,
    /// Durably qualified models on a settled route.
    Balanced,
    /// Signed evidence only, one dispatch per qualification, and no declared
    /// trust at all.
    HighAssurance,
}

impl AssuranceProfile {
    /// Every profile, for exhaustive tests and operator enumeration.
    pub const ALL: [Self; 3] = [Self::Economy, Self::Balanced, Self::HighAssurance];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::HighAssurance => "high_assurance",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "economy" => Some(Self::Economy),
            "balanced" => Some(Self::Balanced),
            "high_assurance" => Some(Self::HighAssurance),
            _ => None,
        }
    }

    /// The profile's ceilings. A `const` table, not a computed policy.
    pub fn ceilings(self) -> AssuranceCeilings {
        match self {
            Self::Economy => AssuranceCeilings {
                max_qualification_age_secs: 5 * 60,
                max_dispatches_per_qualification: 8,
                minimum_action_evidence: QualificationEvidenceKind::Measured,
                honours_declared_trust: true,
            },
            Self::Balanced => AssuranceCeilings {
                max_qualification_age_secs: 15 * 60,
                max_dispatches_per_qualification: 32,
                minimum_action_evidence: QualificationEvidenceKind::Measured,
                honours_declared_trust: true,
            },
            Self::HighAssurance => AssuranceCeilings {
                max_qualification_age_secs: 2 * 60,
                max_dispatches_per_qualification: 1,
                minimum_action_evidence: QualificationEvidenceKind::Signed,
                honours_declared_trust: false,
            },
        }
    }

    pub fn honours_declared_trust(self) -> bool {
        self.ceilings().honours_declared_trust
    }

    /// Whether `evidence` is strong enough for durable action authority here.
    pub fn admits_action_evidence(self, evidence: QualificationEvidenceKind) -> bool {
        evidence >= self.ceilings().minimum_action_evidence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_only_narrows_the_kernel() {
        for profile in AssuranceProfile::ALL {
            profile
                .ceilings()
                .validate()
                .unwrap_or_else(|error| panic!("{profile:?}: {error}"));
        }
    }

    #[test]
    fn no_profile_accepts_action_on_absent_or_declared_evidence() {
        for profile in AssuranceProfile::ALL {
            assert!(!profile.admits_action_evidence(QualificationEvidenceKind::Absent));
            assert!(!profile.admits_action_evidence(QualificationEvidenceKind::Declared));
        }
    }

    #[test]
    fn high_assurance_is_the_strictest_on_every_axis_it_moves() {
        let high = AssuranceProfile::HighAssurance.ceilings();
        for profile in [AssuranceProfile::Economy, AssuranceProfile::Balanced] {
            let other = profile.ceilings();
            assert!(high.max_qualification_age_secs <= other.max_qualification_age_secs);
            assert!(
                high.max_dispatches_per_qualification <= other.max_dispatches_per_qualification
            );
            assert!(high.minimum_action_evidence >= other.minimum_action_evidence);
        }
        assert!(!high.honours_declared_trust);
    }

    #[test]
    fn profile_names_round_trip_and_reject_unknown_spellings() {
        for profile in AssuranceProfile::ALL {
            assert_eq!(AssuranceProfile::parse(profile.as_str()), Some(profile));
        }
        assert_eq!(AssuranceProfile::parse("frontier"), None);
        assert_eq!(AssuranceProfile::parse(""), None);
    }
}
