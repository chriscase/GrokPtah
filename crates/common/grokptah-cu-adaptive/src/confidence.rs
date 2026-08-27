//! Confidence, ambiguity, and the disposition ladder.
//!
//! Two numbers decide whether a step may be committed: how sure the proposer
//! is about the *right* target, and how far ahead of the runner-up that target
//! is. They answer different questions -- a model can be 95% sure while two
//! candidates sit at 95% and 94%, which is not confidence, it is a coin toss
//! with good posture -- so both are thresholded, and the margin test is
//! applied after the confidence test rather than folded into it.
//!
//! Both are basis points. Nothing here is a float: a benchmark that has to
//! reproduce byte-for-byte across platforms cannot afford threshold
//! comparisons that depend on rounding.
//!
//! ## The ladder is ordered so that confidence only ever relaxes it
//!
//! [`Disposition`] is totally ordered by strictness, and
//! [`DecisionThresholds::decide`] is monotone against that order: raising
//! confidence, with everything else held fixed, never produces a stricter
//! disposition. The ordering is chosen to make that true --
//! `RequestApproval` sits *below* `Escalate` because the run is more likely
//! to survive a human gate than a hand-off, and a ladder that jumped back and
//! forth would make "more confident" mean "more blocked" somewhere in the
//! middle. `tests/cu_adaptive_thresholds.rs` sweeps the whole basis-point grid
//! against this property.
//!
//! Mandatory approval gates are deliberately *not* on this ladder. A gate is a
//! property of the step (irreversible, pointer fallback, sensitive-adjacent
//! text, key chord), not of how sure anyone is, so it is carried separately
//! and unioned rather than maximized -- see [`crate::gates`]. Nothing in this
//! module can drop one.

use serde::{Deserialize, Serialize};

use crate::tier::{BPS_FULL, Bps};
use crate::vocabulary::{ApprovalReason, DenyReason, EscalationReason};

/// How hard a step is to undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    /// Undoable within the application.
    Reversible,
    /// Undoable, but only by a compensating action that may itself fail.
    Recoverable,
    /// Not undoable. Always gated.
    Irreversible,
}

impl Reversibility {
    pub const ALL: &'static [Reversibility] =
        &[Self::Reversible, Self::Recoverable, Self::Irreversible];
}

/// What the proposer believes about the candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbiguityAssessment {
    /// How many elements plausibly satisfy the step.
    pub candidate_count: u32,
    /// Confidence in the chosen candidate.
    pub top_confidence_bps: Bps,
    /// Confidence in the best alternative, or zero when there is none.
    pub runner_up_confidence_bps: Bps,
}

impl AmbiguityAssessment {
    /// A single unambiguous candidate at the given confidence.
    #[must_use]
    pub fn unambiguous(top_confidence_bps: Bps) -> Self {
        Self {
            candidate_count: 1,
            top_confidence_bps,
            runner_up_confidence_bps: 0,
        }
    }

    /// Distance between the chosen candidate and the best alternative.
    #[must_use]
    pub fn margin_bps(&self) -> Bps {
        self.top_confidence_bps
            .saturating_sub(self.runner_up_confidence_bps)
    }

    /// True when the assessment is internally possible. An assessment that
    /// claims a runner-up above the top, more confidence than exists, or a
    /// runner-up with no second candidate, is a schema violation rather than a
    /// low-confidence proposal.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.candidate_count >= 1
            && self.top_confidence_bps <= BPS_FULL
            && self.runner_up_confidence_bps <= self.top_confidence_bps
            && (self.candidate_count > 1 || self.runner_up_confidence_bps == 0)
    }
}

/// What to do about a step, ordered from least to most conservative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum Disposition {
    /// Dispatch the step against the live frame.
    Commit,
    /// Gather more evidence -- re-observe, capture a region -- and re-decide.
    Disambiguate,
    /// Ask a human before proceeding.
    RequestApproval { reason: ApprovalReason },
    /// Hand the step to a stronger model.
    Escalate { reason: EscalationReason },
    /// Refuse the step.
    Refuse { reason: DenyReason },
}

impl Disposition {
    /// Position on the strictness ladder. Higher is more conservative.
    #[must_use]
    pub fn strictness(&self) -> u8 {
        match self {
            Self::Commit => 0,
            Self::Disambiguate => 1,
            Self::RequestApproval { .. } => 2,
            Self::Escalate { .. } => 3,
            Self::Refuse { .. } => 4,
        }
    }

    /// True when this disposition dispatches anything at the backend.
    #[must_use]
    pub fn commits(&self) -> bool {
        matches!(self, Self::Commit)
    }

    /// Combine two independent dispositions conservatively.
    ///
    /// The strictest wins. When both sit on the same rung, the reason with the
    /// lower vocabulary ordinal wins, so resolution is deterministic and
    /// independent of which side is asked first.
    #[must_use]
    pub fn resolve(self, other: Disposition) -> Disposition {
        match self.strictness().cmp(&other.strictness()) {
            std::cmp::Ordering::Greater => self,
            std::cmp::Ordering::Less => other,
            std::cmp::Ordering::Equal => match (self, other) {
                (Self::Refuse { reason: a }, Self::Refuse { reason: b }) => {
                    Self::Refuse { reason: a.min(b) }
                }
                (Self::Escalate { reason: a }, Self::Escalate { reason: b }) => {
                    Self::Escalate { reason: a.min(b) }
                }
                (Self::RequestApproval { reason: a }, Self::RequestApproval { reason: b }) => {
                    Self::RequestApproval { reason: a.min(b) }
                }
                _ => self,
            },
        }
    }
}

/// The thresholds one profile applies to one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionThresholds {
    /// Below this, the step is refused outright rather than handed anywhere.
    pub abstain_below_bps: Bps,
    /// Below this (and at or above `abstain_below_bps`), hand upward.
    pub escalate_below_bps: Bps,
    /// The floor for committing a reversible step.
    pub commit_floor_bps: Bps,
    /// The floor for committing an irreversible step. Never below
    /// `commit_floor_bps`.
    pub irreversible_commit_floor_bps: Bps,
    /// The chosen candidate must lead the runner-up by at least this much.
    pub min_margin_bps: Bps,
    /// More plausible candidates than this and the step must disambiguate
    /// before it may commit, however confident it is.
    pub max_candidates: u32,
    /// Whether a confident-but-below-floor step may be committed by a human
    /// instead of refused.
    pub allow_low_confidence_with_approval: bool,
}

impl DecisionThresholds {
    /// True when the thresholds are ordered coherently. An incoherent set
    /// would make the ladder non-monotone, so this is checked at construction
    /// of every profile.
    #[must_use]
    pub fn is_coherent(&self) -> bool {
        self.abstain_below_bps <= self.escalate_below_bps
            && self.escalate_below_bps <= self.commit_floor_bps
            && self.commit_floor_bps <= self.irreversible_commit_floor_bps
            && self.irreversible_commit_floor_bps <= BPS_FULL
            && self.min_margin_bps <= BPS_FULL
            && self.max_candidates >= 1
    }

    /// The confidence floor a step of this reversibility must clear.
    #[must_use]
    pub fn floor_for(&self, reversibility: Reversibility) -> Bps {
        match reversibility {
            Reversibility::Reversible => self.commit_floor_bps,
            // A recoverable step sits halfway: the compensating action can
            // fail, so it is held to a floor between the two rather than to
            // the reversible one.
            Reversibility::Recoverable => self.commit_floor_bps.saturating_add(
                self.irreversible_commit_floor_bps
                    .saturating_sub(self.commit_floor_bps)
                    / 2,
            ),
            Reversibility::Irreversible => self.irreversible_commit_floor_bps,
        }
    }

    /// Decide what to do with a step on confidence grounds alone.
    ///
    /// Grounding, budget, lease, sensitivity, and mandatory gates are decided
    /// elsewhere and resolved against this result. This function answers one
    /// question: given how sure the proposer is, may it act?
    #[must_use]
    pub fn decide(
        &self,
        assessment: &AmbiguityAssessment,
        reversibility: Reversibility,
        already_disambiguated: bool,
    ) -> Disposition {
        if !assessment.is_well_formed() {
            return Disposition::Refuse {
                reason: DenyReason::SchemaViolation,
            };
        }
        let top = assessment.top_confidence_bps;
        if top < self.abstain_below_bps {
            return Disposition::Refuse {
                reason: DenyReason::ConfidenceBelowThreshold,
            };
        }
        if top < self.escalate_below_bps {
            return Disposition::Escalate {
                reason: EscalationReason::AmbiguityUnresolved,
            };
        }
        if top < self.floor_for(reversibility) {
            return if self.allow_low_confidence_with_approval {
                Disposition::RequestApproval {
                    reason: ApprovalReason::LowConfidenceCommit,
                }
            } else {
                Disposition::Refuse {
                    reason: DenyReason::ConfidenceBelowThreshold,
                }
            };
        }
        if assessment.candidate_count > self.max_candidates
            || assessment.margin_bps() < self.min_margin_bps
        {
            // One round of evidence-gathering is free; a second would be the
            // proposer insisting rather than looking, so it hands upward.
            return if already_disambiguated {
                Disposition::Escalate {
                    reason: EscalationReason::AmbiguityUnresolved,
                }
            } else {
                Disposition::Disambiguate
            };
        }
        Disposition::Commit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> DecisionThresholds {
        DecisionThresholds {
            abstain_below_bps: 2_000,
            escalate_below_bps: 5_000,
            commit_floor_bps: 7_000,
            irreversible_commit_floor_bps: 9_000,
            min_margin_bps: 1_000,
            max_candidates: 2,
            allow_low_confidence_with_approval: true,
        }
    }

    #[test]
    fn resolution_is_commutative_idempotent_and_conservative() {
        let ladder = [
            Disposition::Commit,
            Disposition::Disambiguate,
            Disposition::RequestApproval {
                reason: ApprovalReason::PointerFallback,
            },
            Disposition::Escalate {
                reason: EscalationReason::CapabilityGap,
            },
            Disposition::Refuse {
                reason: DenyReason::StaleFrame,
            },
        ];
        for a in ladder {
            assert_eq!(a.resolve(a), a, "resolve is not idempotent for {a:?}");
            for b in ladder {
                let ab = a.resolve(b);
                let ba = b.resolve(a);
                assert_eq!(ab, ba, "resolve is not commutative for {a:?} / {b:?}");
                assert!(ab.strictness() >= a.strictness());
                assert!(ab.strictness() >= b.strictness());
            }
        }
    }

    #[test]
    fn same_rung_conflicts_resolve_deterministically() {
        let a = Disposition::Refuse {
            reason: DenyReason::BudgetExhausted,
        };
        let b = Disposition::Refuse {
            reason: DenyReason::StaleFrame,
        };
        // StaleFrame is declared first, so it wins regardless of argument order.
        assert_eq!(a.resolve(b), b);
        assert_eq!(b.resolve(a), b);
    }

    #[test]
    fn confidence_never_makes_the_ladder_stricter() {
        let thresholds = thresholds();
        for reversibility in Reversibility::ALL {
            for disambiguated in [false, true] {
                let mut previous = u8::MAX;
                let mut bps = 0;
                while bps <= BPS_FULL {
                    let assessment = AmbiguityAssessment::unambiguous(bps);
                    let strictness = thresholds
                        .decide(&assessment, *reversibility, disambiguated)
                        .strictness();
                    assert!(
                        strictness <= previous,
                        "raising confidence to {bps} bps made {reversibility:?} stricter"
                    );
                    previous = strictness;
                    bps += 25;
                }
            }
        }
    }

    #[test]
    fn irreversible_steps_are_held_to_a_higher_floor() {
        let thresholds = thresholds();
        let assessment = AmbiguityAssessment::unambiguous(7_500);
        assert_eq!(
            thresholds.decide(&assessment, Reversibility::Reversible, false),
            Disposition::Commit
        );
        assert_eq!(
            thresholds.decide(&assessment, Reversibility::Irreversible, false),
            Disposition::RequestApproval {
                reason: ApprovalReason::LowConfidenceCommit
            }
        );
        assert!(
            thresholds.floor_for(Reversibility::Recoverable)
                > thresholds.floor_for(Reversibility::Reversible)
        );
    }

    #[test]
    fn a_confident_coin_toss_does_not_commit() {
        let thresholds = thresholds();
        let toss = AmbiguityAssessment {
            candidate_count: 2,
            top_confidence_bps: 9_500,
            runner_up_confidence_bps: 9_400,
        };
        assert_eq!(
            thresholds.decide(&toss, Reversibility::Reversible, false),
            Disposition::Disambiguate
        );
        // Looking twice and still not separating them is a hand-off, not a
        // third look.
        assert_eq!(
            thresholds.decide(&toss, Reversibility::Reversible, true),
            Disposition::Escalate {
                reason: EscalationReason::AmbiguityUnresolved
            }
        );
    }

    #[test]
    fn impossible_assessments_are_schema_violations() {
        let thresholds = thresholds();
        let impossible = AmbiguityAssessment {
            candidate_count: 1,
            top_confidence_bps: 5_000,
            runner_up_confidence_bps: 6_000,
        };
        assert_eq!(
            thresholds.decide(&impossible, Reversibility::Reversible, false),
            Disposition::Refuse {
                reason: DenyReason::SchemaViolation
            }
        );
    }

    #[test]
    fn refusing_instead_of_gating_is_the_stricter_configuration() {
        let strict = DecisionThresholds {
            allow_low_confidence_with_approval: false,
            ..thresholds()
        };
        let assessment = AmbiguityAssessment::unambiguous(7_500);
        assert_eq!(
            strict.decide(&assessment, Reversibility::Irreversible, false),
            Disposition::Refuse {
                reason: DenyReason::ConfidenceBelowThreshold
            }
        );
    }
}
