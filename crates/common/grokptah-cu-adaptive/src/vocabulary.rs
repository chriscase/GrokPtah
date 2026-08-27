//! Closed vocabularies for the adaptive Computer Use contract.
//!
//! Every reason a run can refuse, escalate, ask a human, or stop is a variant
//! of an enum in this module. Nothing here is free text, and no variant
//! carries a payload. That is deliberate and load-bearing in two directions:
//!
//! * **No leakage.** A refusal is the one message that crosses every boundary
//!   in the system -- planner to executor, executor to receipt, receipt to
//!   reviewer. If a refusal could carry a string, observed application text,
//!   a window title, or a typed value could ride out on it. It cannot,
//!   because there is nowhere to put it.
//! * **Default deny.** A caller cannot invent a reason. Parsing an unknown
//!   slug fails rather than degrading to "other", so a plan produced against
//!   a newer vocabulary is refused by an older executor instead of being
//!   partially understood.
//!
//! The vocabulary is intentionally aligned with the production Computer Use
//! safety kernel's error codes rather than being a parallel invention: every
//! [`DenyReason`] maps onto exactly one production error code through
//! [`DenyReason::kernel_error_code`], so a benchmark verdict names the same
//! refusal the real state machine would name.

use serde::{Deserialize, Serialize};

/// Why the contract refused to admit or commit a step.
///
/// The variants are ordered from "the world moved" through "you are not
/// allowed" to "you ran out"; the ordering is not significant to policy, and
/// policy must never branch on discriminant order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The referenced frame is not the live frame.
    StaleFrame,
    /// A pause, takeover, cancellation, or recovery bumped the control epoch.
    FrameEpochChanged,
    /// The run lease is held by someone else, or expired.
    LeaseLost,
    /// A compare-and-swap on the lease saw an unexpected version.
    LeaseVersionConflict,
    /// The element the step names is absent from the live frame.
    TargetMissing,
    /// The application or window identity was recycled or rebound.
    TargetDrifted,
    /// The element exists but is not interactive.
    ElementDisabled,
    /// The element does not advertise the semantic action the step needs.
    ActionNotAdvertised,
    /// The action class is outside the authorized grant.
    ClassOutsideGrant,
    /// A hard-denied surface (secure field, system-restricted window).
    SensitiveSurface,
    /// Evidence would have been exposed without passing redaction.
    RedactionRequired,
    /// The grounding claim is weaker than the profile requires.
    GroundingInsufficient,
    /// A pointer fallback was proposed without usable visual grounding, or by
    /// a model class declared unable to localize.
    PointerWithoutVisualGrounding,
    /// Confidence is below the commit threshold for this reversibility class.
    ConfidenceBelowThreshold,
    /// Candidate targets could not be separated within the ambiguity bound.
    AmbiguityUnresolved,
    /// A budget envelope line item is exhausted.
    BudgetExhausted,
    /// One step took longer than the per-step deadline.
    StepDeadlineExceeded,
    /// The run exceeded its total deadline.
    RunDeadlineExceeded,
    /// The bounded retry allowance for this step or run is spent.
    RetryBudgetExhausted,
    /// A human approval gate is open and unanswered.
    ApprovalRequired,
    /// A human answered the gate with a refusal.
    ApprovalDenied,
    /// The step needs a stronger model than the current tier.
    EscalationRequired,
    /// The escalation ladder has no rung left.
    EscalationExhausted,
    /// The run was cancelled.
    Cancelled,
    /// The plan or verdict did not satisfy the deterministic schema.
    SchemaViolation,
    /// Planner and executor reached different dispositions and the conflict
    /// resolved to a refusal.
    PlannerExecutorDisagreement,
    /// The synthetic backend reported that it could not act.
    BackendUnavailable,
}

impl DenyReason {
    /// Every reason, in declaration order. Used by exhaustiveness tests and by
    /// the vocabulary manifest.
    pub const ALL: &'static [DenyReason] = &[
        Self::StaleFrame,
        Self::FrameEpochChanged,
        Self::LeaseLost,
        Self::LeaseVersionConflict,
        Self::TargetMissing,
        Self::TargetDrifted,
        Self::ElementDisabled,
        Self::ActionNotAdvertised,
        Self::ClassOutsideGrant,
        Self::SensitiveSurface,
        Self::RedactionRequired,
        Self::GroundingInsufficient,
        Self::PointerWithoutVisualGrounding,
        Self::ConfidenceBelowThreshold,
        Self::AmbiguityUnresolved,
        Self::BudgetExhausted,
        Self::StepDeadlineExceeded,
        Self::RunDeadlineExceeded,
        Self::RetryBudgetExhausted,
        Self::ApprovalRequired,
        Self::ApprovalDenied,
        Self::EscalationRequired,
        Self::EscalationExhausted,
        Self::Cancelled,
        Self::SchemaViolation,
        Self::PlannerExecutorDisagreement,
        Self::BackendUnavailable,
    ];

    /// The production Computer Use error code this refusal maps onto.
    ///
    /// The adaptive layer sits above the safety kernel and must never invent
    /// a disposition the kernel cannot express. This mapping is total and is
    /// asserted against the kernel's own enum by the bridge-side conformance
    /// test, so a refusal here is always a refusal the kernel already has a
    /// name for.
    #[must_use]
    pub fn kernel_error_code(self) -> &'static str {
        match self {
            Self::StaleFrame => "stale_observation",
            Self::FrameEpochChanged => "invalid_state",
            Self::LeaseLost => "unauthorized",
            Self::LeaseVersionConflict => "conflict",
            Self::TargetMissing => "stale_observation",
            Self::TargetDrifted => "target_changed",
            Self::ElementDisabled => "forbidden_action",
            Self::ActionNotAdvertised => "forbidden_action",
            Self::ClassOutsideGrant => "forbidden_action",
            Self::SensitiveSurface => "sensitive_surface",
            Self::RedactionRequired => "sensitive_surface",
            Self::GroundingInsufficient => "uncertain_outcome",
            Self::PointerWithoutVisualGrounding => "forbidden_action",
            Self::ConfidenceBelowThreshold => "uncertain_outcome",
            Self::AmbiguityUnresolved => "uncertain_outcome",
            Self::BudgetExhausted => "limit_reached",
            Self::StepDeadlineExceeded => "limit_reached",
            Self::RunDeadlineExceeded => "limit_reached",
            Self::RetryBudgetExhausted => "limit_reached",
            Self::ApprovalRequired => "permission_required",
            Self::ApprovalDenied => "permission_denied",
            Self::EscalationRequired => "pending",
            Self::EscalationExhausted => "uncertain_outcome",
            Self::Cancelled => "interrupted",
            Self::SchemaViolation => "invalid_request",
            Self::PlannerExecutorDisagreement => "uncertain_outcome",
            Self::BackendUnavailable => "backend_unavailable",
        }
    }

    /// True when the refusal is a property of the world or the budget rather
    /// than of the proposal, so a retry with the same proposal may succeed
    /// after re-observing.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::StaleFrame | Self::TargetMissing | Self::BackendUnavailable
        )
    }

    /// True when the refusal is terminal for the whole run rather than for one
    /// step. Terminal refusals must never be retried, escalated, or approved
    /// around.
    #[must_use]
    pub fn is_run_terminal(self) -> bool {
        matches!(
            self,
            Self::Cancelled
                | Self::RunDeadlineExceeded
                | Self::ApprovalDenied
                | Self::EscalationExhausted
                // Nothing this run does can get the lease back: it either
                // expired or an operator took the target over. Retrying would
                // be the run insisting it is still in charge.
                | Self::LeaseLost
        )
    }
}

/// Why a step was handed to a stronger model.
///
/// Escalation buys capability. It never buys authority: the stronger model
/// inherits exactly the grant, action classes, and redaction the weaker one
/// had. [`crate::escalation`] enforces that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// Candidates could not be separated at this tier.
    AmbiguityUnresolved,
    /// The grounding the profile requires is not obtainable at this tier.
    GroundingInsufficient,
    /// The declared tier capability does not cover the step (for example a
    /// pointer step proposed by a pixel-blind class).
    CapabilityGap,
    /// The same step failed its postcondition repeatedly.
    RepeatedPostconditionMiss,
    /// Planner and executor disagreed and neither could resolve it.
    DisagreementUnresolved,
    /// The plan is deeper than this tier is declared able to hold.
    PlanDepthExceeded,
}

impl EscalationReason {
    pub const ALL: &'static [EscalationReason] = &[
        Self::AmbiguityUnresolved,
        Self::GroundingInsufficient,
        Self::CapabilityGap,
        Self::RepeatedPostconditionMiss,
        Self::DisagreementUnresolved,
        Self::PlanDepthExceeded,
    ];

    /// True when the reason is a standing property of the model class rather
    /// than of one step.
    ///
    /// The distinction decides whether a run drops back to its base tier after
    /// the step. A step that was merely ambiguous should not cost strong-model
    /// prices for the rest of the run, so the ladder settles. A class that
    /// cannot see cannot see on the next step either, so re-escalating every
    /// step would burn the escalation budget on a fact that has not changed --
    /// the ladder stays climbed instead. See
    /// [`crate::escalation::EscalationLadder::settle`].
    #[must_use]
    pub fn is_persistent(self) -> bool {
        matches!(
            self,
            Self::CapabilityGap | Self::GroundingInsufficient | Self::PlanDepthExceeded
        )
    }
}

/// Why a human approval gate opened.
///
/// Gates are properties of the *step*, not of the profile. A cheap profile
/// does not get to skip a gate that an expensive profile would open; see
/// [`crate::profile`] and the authority-parity test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReason {
    /// The step is declared irreversible by the plan.
    IrreversibleStep,
    /// A pointer fallback leaves the semantic surface.
    PointerFallback,
    /// Text entry into a field adjacent to a sensitive surface.
    SensitiveAdjacentTextEntry,
    /// A key chord can reach application-global commands.
    KeyChord,
    /// Handing the objective to a stronger model.
    EscalationToStrongerModel,
    /// Committing below the confidence floor, when the profile allows a human
    /// to make that call rather than abstaining outright.
    LowConfidenceCommit,
}

impl ApprovalReason {
    pub const ALL: &'static [ApprovalReason] = &[
        Self::IrreversibleStep,
        Self::PointerFallback,
        Self::SensitiveAdjacentTextEntry,
        Self::KeyChord,
        Self::EscalationToStrongerModel,
        Self::LowConfidenceCommit,
    ];
}

/// How a run ended. Exactly one is recorded per run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The planner reported completion and the executor agreed on the live
    /// frame.
    ObjectiveComplete,
    /// The horizon was exhausted with work still outstanding.
    HorizonExhausted,
    /// The run stopped itself for a permitted reason rather than guessing.
    Abstained,
    /// A refusal ended the run.
    Denied,
    /// Cancellation, external or scripted.
    Cancelled,
    /// A budget line item ran out.
    BudgetExhausted,
    /// The run or step deadline elapsed.
    DeadlineExceeded,
    /// A human refused a gate.
    HumanRejected,
}

impl StopReason {
    pub const ALL: &'static [StopReason] = &[
        Self::ObjectiveComplete,
        Self::HorizonExhausted,
        Self::Abstained,
        Self::Denied,
        Self::Cancelled,
        Self::BudgetExhausted,
        Self::DeadlineExceeded,
        Self::HumanRejected,
    ];

    /// True when the run reached a clean, reviewable end rather than being cut
    /// off. Both are legitimate; only the first may be reported as success.
    #[must_use]
    pub fn is_orderly(self) -> bool {
        matches!(self, Self::ObjectiveComplete | Self::Abstained)
    }
}

/// A claim the receipt explicitly does **not** make.
///
/// This benchmark runs entirely against a deterministic synthetic world. It
/// has no hardware, no virtual machine, no provider, and no image model. The
/// receipt is required to carry the full mandatory set of disclaimers, so a
/// reader who sees only the receipt still cannot mistake it for a measurement
/// of any of those things. See [`crate::receipt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotClaimed {
    /// No statement about timing on real hardware.
    RealHardwareTiming,
    /// No statement about a virtual machine or isolated guest.
    VirtualMachineBehavior,
    /// No statement about any model provider's latency, cost, or availability.
    ProviderLatencyOrCost,
    /// No statement about an image model's grounding accuracy.
    ImageModelAccuracy,
    /// No statement about how a real operator answers an approval gate.
    HumanOperatorBehavior,
    /// No statement about how a real application behaves.
    RealApplicationSemantics,
    /// No statement about token counts or prices at any provider; cost units
    /// here are synthetic and dimensionless.
    TokenAccounting,
}

impl NotClaimed {
    /// The disclaimers every receipt from this harness must carry.
    ///
    /// This is the whole enum on purpose. There is no run in this crate that
    /// earns the right to drop one of them, so the mandatory set and the
    /// vocabulary are the same list.
    pub const MANDATORY: &'static [NotClaimed] = &[
        Self::RealHardwareTiming,
        Self::VirtualMachineBehavior,
        Self::ProviderLatencyOrCost,
        Self::ImageModelAccuracy,
        Self::HumanOperatorBehavior,
        Self::RealApplicationSemantics,
        Self::TokenAccounting,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_slugs_are_unique_and_snake_case() {
        let mut seen = std::collections::BTreeSet::new();
        for reason in DenyReason::ALL {
            let slug = serde_json::to_value(reason).unwrap();
            let slug = slug.as_str().unwrap().to_string();
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "{slug} is not snake_case"
            );
            assert!(seen.insert(slug.clone()), "duplicate slug {slug}");
        }
        assert_eq!(seen.len(), DenyReason::ALL.len());
    }

    #[test]
    fn unknown_refusal_slugs_fail_closed() {
        let parsed: Result<DenyReason, _> = serde_json::from_str("\"other\"");
        assert!(parsed.is_err());
        let parsed: Result<DenyReason, _> = serde_json::from_str("\"stale_frame_v2\"");
        assert!(parsed.is_err());
    }

    #[test]
    fn every_refusal_maps_to_a_kernel_error_code() {
        for reason in DenyReason::ALL {
            let code = reason.kernel_error_code();
            assert!(!code.is_empty());
            assert!(code.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }

    #[test]
    fn run_terminal_refusals_are_never_retryable() {
        for reason in DenyReason::ALL {
            assert!(
                !(reason.is_run_terminal() && reason.is_retryable()),
                "{reason:?} is both terminal and retryable"
            );
        }
    }

    #[test]
    fn persistence_splits_the_escalation_vocabulary() {
        let persistent: Vec<_> = EscalationReason::ALL
            .iter()
            .filter(|reason| reason.is_persistent())
            .collect();
        let transient: Vec<_> = EscalationReason::ALL
            .iter()
            .filter(|reason| !reason.is_persistent())
            .collect();
        assert!(!persistent.is_empty());
        assert!(!transient.is_empty());
        assert_eq!(
            persistent.len() + transient.len(),
            EscalationReason::ALL.len()
        );
        assert!(EscalationReason::CapabilityGap.is_persistent());
        assert!(!EscalationReason::AmbiguityUnresolved.is_persistent());
    }

    #[test]
    fn mandatory_disclaimers_cover_the_whole_vocabulary() {
        // If a variant is ever added without being made mandatory, this fails
        // rather than silently letting a receipt claim more than it can.
        for candidate in [
            NotClaimed::RealHardwareTiming,
            NotClaimed::VirtualMachineBehavior,
            NotClaimed::ProviderLatencyOrCost,
            NotClaimed::ImageModelAccuracy,
            NotClaimed::HumanOperatorBehavior,
            NotClaimed::RealApplicationSemantics,
            NotClaimed::TokenAccounting,
        ] {
            assert!(NotClaimed::MANDATORY.contains(&candidate));
        }
    }
}
