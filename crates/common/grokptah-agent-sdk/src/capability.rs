//! Versioned capability discovery contracts.

use serde::{Deserialize, Serialize};

use crate::CONTRACT_VERSION;

/// Authority tier advertised to a consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityTier {
    /// Redacted session, capacity, and event observation.
    Observe,
    /// Bounded run or queue mutations within an approved scope.
    Execute,
    /// Bounded changes, tests, handoff, and review projections.
    Review,
    /// Human-gated approval, promotion, or discard of an isolated run.
    Promote,
    /// Redacted Computer Use projections and audit events.
    ComputerObserve,
    /// Lease- and revision-fenced Computer Use controls.
    ComputerControl,
}

/// Whether a host can expose a capability to the negotiated consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    /// Available subject to ordinary scope and authentication checks.
    Available,
    /// Present but requires an explicit human or lease grant.
    Gated,
    /// Known to the contract but unavailable on this host.
    Unavailable,
}

/// One stable capability descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    /// Stable identifier such as `run.execute`.
    pub id: String,
    /// Authority tier for the capability.
    pub tier: CapabilityTier,
    /// Whether an operation can change durable or external state.
    pub mutating: bool,
    /// Whether a separate grant is required.
    pub human_gate: bool,
    /// Host availability after negotiation.
    pub availability: CapabilityAvailability,
    /// Share-safe description for a consumer UI.
    pub description: String,
}

/// Versioned capability discovery payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Contract identifier. Must equal [`CONTRACT_VERSION`] for this module.
    pub contract: String,
    /// Capabilities advertised by the host.
    pub capabilities: Vec<CapabilityDescriptor>,
}

impl CapabilitySet {
    /// Construct an empty set for the current contract version.
    pub fn empty() -> Self {
        Self {
            contract: CONTRACT_VERSION.to_owned(),
            capabilities: Vec::new(),
        }
    }

    /// Find a capability by its stable identifier.
    pub fn get(&self, id: &str) -> Option<&CapabilityDescriptor> {
        self.capabilities
            .iter()
            .find(|capability| capability.id == id)
    }

    /// Reject an unknown contract before a consumer enables any operation.
    pub fn is_current(&self) -> bool {
        self.contract == CONTRACT_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_is_current_and_round_trippable() {
        let set = CapabilitySet::empty();
        assert!(set.is_current());
        let encoded = serde_json::to_vec(&set).expect("capability set serializes");
        let decoded: CapabilitySet =
            serde_json::from_slice(&encoded).expect("capability set parses");
        assert_eq!(decoded, set);
    }
}
