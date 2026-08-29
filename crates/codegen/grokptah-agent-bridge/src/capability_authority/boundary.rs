//! The boundaries a capability binding is re-validated at.
//!
//! A qualification is taken once and then used many times, across minutes, by
//! several surfaces. Every one of those uses is a place where the capability
//! could already have changed, so each is named here and each re-validates.
//! Nothing in a Computer Use session reaches a model or a screen without
//! passing exactly one of these.

use serde::{Deserialize, Serialize};

use crate::gateway_config::ComputerUseTier;

/// One place a capability binding is presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityBoundary {
    /// Recording the qualification itself.
    Qualification,
    /// Taking a fresh observation of the screen under model authority.
    Observation,
    /// Asking the model for one semantic proposal.
    Proposal,
    /// Staging a proposed action for local approval.
    Staging,
    /// Resolving a local approval into an authorized action.
    Approval,
    /// Acquiring the target lease an action will be dispatched through.
    Lease,
    /// Delivering one live observation frame to a model-attributed consumer.
    LiveFrame,
    /// The physical action reaching the backend.
    Dispatch,
}

impl CapabilityBoundary {
    /// Every boundary, for exhaustive tests.
    pub const ALL: [Self; 8] = [
        Self::Qualification,
        Self::Observation,
        Self::Proposal,
        Self::Staging,
        Self::Approval,
        Self::Lease,
        Self::LiveFrame,
        Self::Dispatch,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qualification => "qualification",
            Self::Observation => "observation",
            Self::Proposal => "proposal",
            Self::Staging => "staging",
            Self::Approval => "approval",
            Self::Lease => "lease",
            Self::LiveFrame => "live_frame",
            Self::Dispatch => "dispatch",
        }
    }

    /// The lowest tier that may pass this boundary.
    ///
    /// Observation-class boundaries need observation authority; everything
    /// that leads to a physical action needs semantic action authority. There
    /// is no boundary that a `None` tier passes.
    pub fn required_tier(self) -> ComputerUseTier {
        match self {
            Self::Qualification | Self::Observation | Self::LiveFrame => ComputerUseTier::Observe,
            Self::Proposal | Self::Staging | Self::Approval | Self::Lease | Self::Dispatch => {
                ComputerUseTier::SemanticAct
            }
        }
    }

    /// Whether the authority must re-derive live capability facts at this
    /// boundary rather than comparing against the facts it already holds.
    ///
    /// Dispatch is the last instant before a physical action, and a live frame
    /// is screen content leaving the host; both are re-derived so a downgrade
    /// that lands mid-operation is caught here rather than at the next
    /// operation.
    pub fn requires_live_recheck(self) -> bool {
        matches!(self, Self::Dispatch | Self::LiveFrame)
    }

    /// Whether passing this boundary consumes a dispatch from the
    /// qualification's per-profile budget.
    pub fn consumes_dispatch_budget(self) -> bool {
        matches!(self, Self::Dispatch)
    }
}

/// The boundaries one binding may pass.
///
/// Derived once, from the tier the capability actually resolved to, and then
/// carried by the binding. An observation-only capability's set simply does
/// not contain [`CapabilityBoundary::Dispatch`], so "declared capability
/// became action authority" is not a policy that could be misread — it is a
/// state that cannot be represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundarySet {
    tier: ComputerUseTier,
}

impl BoundarySet {
    pub(super) fn for_tier(tier: ComputerUseTier) -> Self {
        Self { tier }
    }

    pub fn tier(self) -> ComputerUseTier {
        self.tier
    }

    pub fn allows(self, boundary: CapabilityBoundary) -> bool {
        self.tier >= boundary.required_tier() && self.tier > ComputerUseTier::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_boundary_is_open_to_an_unqualified_tier() {
        let none = BoundarySet::for_tier(ComputerUseTier::None);
        for boundary in CapabilityBoundary::ALL {
            assert!(!none.allows(boundary), "{boundary:?}");
        }
    }

    #[test]
    fn observation_authority_never_reaches_an_action_boundary() {
        let observe = BoundarySet::for_tier(ComputerUseTier::Observe);
        assert!(observe.allows(CapabilityBoundary::Observation));
        assert!(observe.allows(CapabilityBoundary::LiveFrame));
        assert!(observe.allows(CapabilityBoundary::Qualification));
        for boundary in [
            CapabilityBoundary::Proposal,
            CapabilityBoundary::Staging,
            CapabilityBoundary::Approval,
            CapabilityBoundary::Lease,
            CapabilityBoundary::Dispatch,
        ] {
            assert!(!observe.allows(boundary), "{boundary:?}");
        }
    }

    #[test]
    fn action_authority_covers_every_boundary() {
        let act = BoundarySet::for_tier(ComputerUseTier::SemanticAct);
        for boundary in CapabilityBoundary::ALL {
            assert!(act.allows(boundary), "{boundary:?}");
        }
    }

    #[test]
    fn dispatch_and_live_frames_are_the_re_derived_boundaries() {
        for boundary in CapabilityBoundary::ALL {
            assert_eq!(
                boundary.requires_live_recheck(),
                matches!(
                    boundary,
                    CapabilityBoundary::Dispatch | CapabilityBoundary::LiveFrame
                ),
                "{boundary:?}"
            );
        }
    }
}
