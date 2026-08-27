//! Profiles buy verification. They never buy authority.
//!
//! This is the claim that makes running one benchmark across three efficiency
//! profiles worth anything: `Economy` is allowed to be worse at the task, and
//! is not allowed to be less safe. It is checked here step by step rather than
//! by comparing whole runs, because whole runs differ in how far they get for
//! entirely legitimate reasons, and a comparison that cannot tell "refused
//! less" from "got less far" cannot catch the bug it exists for.
//!
//! Every hazard below is evaluated under all three profiles and all three
//! model tiers with grounding supplied at the highest level any profile
//! demands, so what is being compared is what each profile *refuses* rather
//! than what each *asks for*.

mod common;

use std::collections::BTreeSet;

use common::{Fixture, full_grounding, live_element};
use grokptah_cu_adaptive::cancel::{CancelCause, Resource};
use grokptah_cu_adaptive::confidence::{Disposition, Reversibility};
use grokptah_cu_adaptive::digest::{digest_str, domain};
use grokptah_cu_adaptive::escalation::EscalationContext;
use grokptah_cu_adaptive::grounding::GroundingClaim;
use grokptah_cu_adaptive::lease::EpochBump;
use grokptah_cu_adaptive::profile::{AUTHORITY_INVARIANTS, ProfileId};
use grokptah_cu_adaptive::redaction::Sensitivity;
use grokptah_cu_adaptive::schema::{ElementRef, IntentFamily, StepIntent};
use grokptah_cu_adaptive::tier::ModelTier;
use grokptah_cu_adaptive::vocabulary::DenyReason;

/// One hazard, and the refusal it must always produce.
struct Hazard {
    name: &'static str,
    expected: DenyReason,
    perturb: fn(&mut Fixture),
}

const HAZARDS: &[Hazard] = &[
    Hazard {
        name: "secure surface",
        expected: DenyReason::SensitiveSurface,
        perturb: |fixture| {
            if let Some(live) = fixture.live_element.as_mut() {
                live.sensitivity = Sensitivity::Secure;
            }
        },
    },
    Hazard {
        name: "system-restricted surface",
        expected: DenyReason::SensitiveSurface,
        perturb: |fixture| {
            if let Some(live) = fixture.live_element.as_mut() {
                live.sensitivity = Sensitivity::SystemRestricted;
            }
        },
    },
    Hazard {
        name: "family outside the grant",
        expected: DenyReason::ClassOutsideGrant,
        perturb: |fixture| {
            fixture.context = EscalationContext::new(BTreeSet::new(), fixture.lease.epoch);
        },
    },
    Hazard {
        name: "disabled control",
        expected: DenyReason::ElementDisabled,
        perturb: |fixture| {
            if let Some(live) = fixture.live_element.as_mut() {
                live.enabled = false;
            }
        },
    },
    Hazard {
        name: "action not advertised",
        expected: DenyReason::ActionNotAdvertised,
        perturb: |fixture| {
            if let Some(live) = fixture.live_element.as_mut() {
                live.advertises = false;
            }
        },
    },
    Hazard {
        name: "element gone",
        expected: DenyReason::TargetMissing,
        perturb: |fixture| fixture.live_element = None,
    },
    Hazard {
        name: "recycled identity",
        expected: DenyReason::TargetDrifted,
        perturb: |fixture| {
            if let Some(live) = fixture.live_element.as_mut() {
                live.element.generation += 1;
            }
        },
    },
    Hazard {
        name: "role changed under the identity",
        expected: DenyReason::TargetDrifted,
        perturb: |fixture| {
            if let Some(live) = fixture.live_element.as_mut() {
                live.role_digest = digest_str(domain::ELEMENT_ROLE, "menu_item");
            }
        },
    },
    Hazard {
        name: "region redrawn",
        expected: DenyReason::StaleFrame,
        perturb: |fixture| {
            if let Some(live) = fixture.live_element.as_mut() {
                live.region_digest = digest_str(domain::REGION, "other-bytes");
            }
        },
    },
    Hazard {
        name: "frame superseded",
        expected: DenyReason::StaleFrame,
        perturb: |fixture| fixture.live_frame.sequence += 1,
    },
    Hazard {
        name: "operator takeover",
        expected: DenyReason::LeaseLost,
        perturb: |fixture| {
            fixture.lease.bump_epoch(EpochBump::OperatorTakeover);
            fixture.context = EscalationContext::new(common::full_grant(), fixture.lease.epoch);
        },
    },
    Hazard {
        name: "cancelled",
        expected: DenyReason::Cancelled,
        perturb: |fixture| {
            fixture.cleanup.acquire(Resource::Lease);
            fixture
                .cleanup
                .cancel(&mut fixture.lease, CancelCause::OperatorRequest, 1);
        },
    },
    Hazard {
        name: "control epoch moved",
        expected: DenyReason::FrameEpochChanged,
        perturb: |fixture| {
            fixture.lease.bump_epoch(EpochBump::Paused);
        },
    },
    Hazard {
        name: "malformed grounding digest",
        expected: DenyReason::SchemaViolation,
        perturb: |fixture| {
            fixture.plan.steps[0].grounding = GroundingClaim::Semantic {
                element: common::element(),
                role_digest: "not-a-digest".into(),
            };
        },
    },
];

#[test]
fn every_hazard_produces_the_same_refusal_under_every_profile() {
    for hazard in HAZARDS {
        for tier in ModelTier::ALL {
            let mut observed = Vec::new();
            for profile in ProfileId::ALL {
                let mut fixture = Fixture::new(*profile, *tier);
                (hazard.perturb)(&mut fixture);
                let verdict = fixture.evaluate();
                observed.push((*profile, verdict.refusal()));
            }
            let first = observed[0].1;
            assert_eq!(
                first,
                Some(hazard.expected),
                "{} at {tier:?} did not refuse as expected under {:?}",
                hazard.name,
                observed[0].0
            );
            for (profile, refusal) in &observed {
                assert_eq!(
                    *refusal, first,
                    "{} at {tier:?} refused differently under {profile:?}",
                    hazard.name
                );
            }
        }
    }
}

#[test]
fn no_profile_and_no_tier_ever_commits_a_hazard() {
    for hazard in HAZARDS {
        for profile in ProfileId::ALL {
            for tier in ModelTier::ALL {
                let mut fixture = Fixture::new(*profile, *tier);
                (hazard.perturb)(&mut fixture);
                // Even with the planner insisting.
                fixture.planner = Disposition::Commit;
                assert!(
                    !fixture.evaluate().commits(),
                    "{} committed under {profile:?}/{tier:?}",
                    hazard.name
                );
            }
        }
    }
}

#[test]
fn every_hazard_refusal_is_a_declared_authority_invariant() {
    // The point of the constant is that it is the list of refusals no profile
    // can suppress. If a hazard here produced something outside it, either the
    // hazard is not an authority matter or the list is incomplete.
    for hazard in HAZARDS {
        assert!(
            AUTHORITY_INVARIANTS.contains(&hazard.expected),
            "{} refuses with {:?}, which is not declared an authority invariant",
            hazard.name,
            hazard.expected
        );
    }
}

#[test]
fn the_pointer_rule_is_a_property_of_the_class_not_of_the_profile() {
    // Unlike the hazards above, this one legitimately differs by tier: a class
    // that can localize may click and one that cannot may not. What must not
    // differ is the profile.
    let pointer = common::step(
        StepIntent::PointerFallback {
            x: 8,
            y: 8,
            button: grokptah_cu_adaptive::schema::PointerButton::Primary,
        },
        Reversibility::Reversible,
    );
    for tier in ModelTier::ALL {
        let mut refusals = Vec::new();
        for profile in ProfileId::ALL {
            let mut fixture = Fixture::with_step(*profile, *tier, pointer.clone());
            fixture.plan.steps[0].grounding = full_grounding();
            fixture.live_element = Some(live_element());
            refusals.push(fixture.evaluate().refusal());
        }
        let expected = if tier.declared().pixel_blind() {
            Some(DenyReason::PointerWithoutVisualGrounding)
        } else {
            None
        };
        for (index, refusal) in refusals.iter().enumerate() {
            assert_eq!(
                *refusal,
                expected,
                "{:?} at {tier:?} disagreed about the pointer rule",
                ProfileId::ALL[index]
            );
        }
    }
}

#[test]
fn confidence_never_unlocks_an_authority_refusal() {
    // Profiles are *supposed* to differ on confidence -- that is the
    // verification they buy. What they may not differ on is authority. So the
    // sweep is over every hazard at every confidence level, and the claim is
    // that whenever the expensive profile refuses for an authority invariant,
    // the cheap one refuses identically, however sure the planner claims to
    // be.
    for hazard in HAZARDS {
        let mut confidence = 0;
        while confidence <= 10_000 {
            for reversibility in Reversibility::ALL {
                let mut refusals = Vec::new();
                for profile in ProfileId::ALL {
                    let mut fixture = Fixture::new(*profile, ModelTier::StrongHosted);
                    fixture.plan.steps[0].ambiguity =
                        grokptah_cu_adaptive::confidence::AmbiguityAssessment::unambiguous(
                            confidence,
                        );
                    fixture.plan.steps[0].reversibility = *reversibility;
                    (hazard.perturb)(&mut fixture);
                    fixture.plan_digest = fixture.plan.digest().unwrap();
                    refusals.push((*profile, fixture.evaluate().refusal()));
                }
                let invariant: Vec<_> = refusals
                    .iter()
                    .map(|(profile, refusal)| {
                        (
                            *profile,
                            refusal.filter(|reason| AUTHORITY_INVARIANTS.contains(reason)),
                        )
                    })
                    .collect();
                let first = invariant[0].1;
                for (profile, refusal) in &invariant {
                    assert_eq!(
                        *refusal, first,
                        "{} at {confidence} bps / {reversibility:?} refused differently under {profile:?}",
                        hazard.name
                    );
                }
                assert_eq!(
                    first,
                    Some(hazard.expected),
                    "{} stopped being refused at {confidence} bps / {reversibility:?}",
                    hazard.name
                );
            }
            confidence += 250;
        }
    }
}

#[test]
fn profiles_do_differ_on_verification() {
    // The other half of the claim, and the reason the parity checks above are
    // not vacuous: if every profile behaved identically on everything, parity
    // would be trivial and the profiles would be pointless. There has to be a
    // confidence at which the cheap profile acts and the expensive one does
    // not.
    let mut found_a_difference = false;
    let mut confidence = 0;
    while confidence <= 10_000 {
        let mut economy = Fixture::new(ProfileId::Economy, ModelTier::StrongHosted);
        economy.plan.steps[0].ambiguity =
            grokptah_cu_adaptive::confidence::AmbiguityAssessment::unambiguous(confidence);
        economy.plan_digest = economy.plan.digest().unwrap();
        let mut assured = Fixture::new(ProfileId::HighAssurance, ModelTier::StrongHosted);
        assured.plan.steps[0].ambiguity =
            grokptah_cu_adaptive::confidence::AmbiguityAssessment::unambiguous(confidence);
        assured.plan_digest = assured.plan.digest().unwrap();
        if economy.evaluate().commits() && !assured.evaluate().commits() {
            found_a_difference = true;
            break;
        }
        confidence += 50;
    }
    assert!(
        found_a_difference,
        "no confidence level separates the cheapest profile from the dearest"
    );
}

#[test]
fn a_grant_narrows_and_never_widens_by_tier() {
    let only_ambient: BTreeSet<IntentFamily> = [IntentFamily::Ambient].into_iter().collect();
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            let mut fixture = Fixture::new(*profile, *tier);
            fixture.context = EscalationContext::new(only_ambient.clone(), 0);
            assert_eq!(
                fixture.evaluate().refusal(),
                Some(DenyReason::ClassOutsideGrant),
                "{profile:?}/{tier:?} acted outside a narrowed grant"
            );
        }
    }
}

#[test]
fn a_hard_denied_surface_outranks_a_granted_approval() {
    for profile in ProfileId::ALL {
        let mut fixture = Fixture::new(*profile, ModelTier::StrongHosted);
        if let Some(live) = fixture.live_element.as_mut() {
            live.sensitivity = Sensitivity::Secure;
        }
        fixture.approval = Some(grokptah_cu_adaptive::gates::ApprovalDecision {
            plan_digest: fixture.plan_digest.clone(),
            step_index: 0,
            granted: grokptah_cu_adaptive::vocabulary::ApprovalReason::ALL.to_vec(),
            approved: true,
            epoch: 0,
        });
        assert_eq!(
            fixture.evaluate().refusal(),
            Some(DenyReason::SensitiveSurface),
            "{profile:?} let a human authorize a hard-denied surface"
        );
    }
}

#[test]
fn a_recycled_element_reference_is_refused_rather_than_matched_by_name() {
    for profile in ProfileId::ALL {
        let mut fixture = Fixture::new(*profile, ModelTier::StrongHosted);
        // Same id, different generation, on both sides of the claim.
        let recycled = ElementRef::new(common::ELEMENT_ID, 9).unwrap();
        fixture.plan.steps[0].intent = StepIntent::Invoke {
            element: recycled.clone(),
        };
        fixture.plan.steps[0].grounding = GroundingClaim::SemanticPlusRegion {
            element: recycled,
            role_digest: common::role_digest(),
            region_digest: common::region_digest(),
        };
        fixture.plan_digest = fixture.plan.digest().unwrap();
        assert_eq!(
            fixture.evaluate().refusal(),
            Some(DenyReason::TargetDrifted),
            "{profile:?} matched a recycled identity by name"
        );
    }
}
