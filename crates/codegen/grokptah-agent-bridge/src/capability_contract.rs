//! Desktop-authority capability advertisement backed by the host-neutral SDK.
//!
//! The bridge owns the policy that decides which tools are present. The DTOs
//! themselves come from `grokptah-agent-sdk` so a ContextDesk adapter and the
//! desktop authority cannot silently serialize different contract versions.

pub use grokptah_agent_sdk::{
    CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier,
};

/// Version of the capability-discovery payload.
pub const CAPABILITY_CONTRACT_VERSION: &str = grokptah_agent_sdk::CONTRACT_VERSION;

use sha2::{Digest, Sha256};

use crate::orchestration::CONTROL_TOOLS;

/// Stable digest of the *authorization-relevant* shape of a capability set.
///
/// Human approval receipts bind this value, so a receipt issued while
/// `computer.control` was advertised one way cannot be redeemed after the
/// advertised authority model changes — a tool appearing or disappearing, a
/// tier or `mutating` flag moving, or a gate being dropped all invalidate
/// outstanding receipts.
///
/// `description` is deliberately excluded: prose churn is not an authority
/// change and must not invalidate a human's decision.
pub fn capability_revision_of(set: &CapabilitySet) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.capability.revision.v1\x00");
    hasher.update(set.contract.as_bytes());
    for capability in &set.capabilities {
        hasher.update(b"\x00");
        hasher.update(capability.id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(format!("{:?}", capability.tier).as_bytes());
        hasher.update(b"\x00");
        hasher.update([u8::from(capability.mutating)]);
        hasher.update([u8::from(capability.human_gate)]);
        hasher.update(b"\x00");
        hasher.update(format!("{:?}", capability.availability).as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Digest of the set this host currently advertises.
pub fn capability_revision() -> String {
    capability_revision_of(&advertised_capabilities())
}

/// Build the advertised set from the bridge's allowlisted control tools.
///
/// This is intentionally derived from `CONTROL_TOOLS`: a consumer cannot be
/// told that an operation exists when the transport does not expose it.
/// Availability remains separate from authorization; promotion and Computer
/// Use control are still advertised as gated.
pub fn advertised_capabilities() -> CapabilitySet {
    let has = |tool: &str| CONTROL_TOOLS.contains(&tool);
    let mut capabilities = Vec::new();

    if has("ptah_list_sessions") && has("ptah_get_capacity") {
        capabilities.push(CapabilityDescriptor {
            id: "session.observe".into(),
            tier: CapabilityTier::Observe,
            mutating: false,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "List sessions and bounded orchestration capacity.".into(),
        });
    }
    if has("ptah_submit_task") && has("ptah_retry_run") && has("ptah_cancel") {
        capabilities.push(CapabilityDescriptor {
            id: "run.execute".into(),
            tier: CapabilityTier::Execute,
            mutating: true,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "Submit, retry, and cancel bounded Build runs.".into(),
        });
    }
    if has("ptah_get_queue")
        && has("ptah_queue_prompt")
        && has("ptah_edit_queue")
        && has("ptah_remove_queue")
        && has("ptah_reorder_queue")
        && has("ptah_clear_queue")
        && has("ptah_run_next")
        && has("ptah_steer_queued")
        && has("ptah_steer")
    {
        capabilities.push(CapabilityDescriptor {
            id: "run.queue".into(),
            tier: CapabilityTier::Execute,
            mutating: true,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "Inspect and mutate a session's versioned prompt queue.".into(),
        });
    }
    if has("ptah_get_changes")
        && has("ptah_get_test_results")
        && has("ptah_get_handoff")
        && has("ptah_review_run")
    {
        capabilities.push(CapabilityDescriptor {
            id: "run.review".into(),
            tier: CapabilityTier::Review,
            mutating: false,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "Read bounded changes, tests, handoffs, and review projections.".into(),
        });
    }
    if has("ptah_approve_run") && has("ptah_promote_run") && has("ptah_discard_run") {
        capabilities.push(CapabilityDescriptor {
            id: "run.promote".into(),
            tier: CapabilityTier::Promote,
            mutating: true,
            human_gate: true,
            availability: CapabilityAvailability::Gated,
            description: "Approve, promote, or discard an isolated run after review.".into(),
        });
    }
    if has("ptah_list_persistent_agents") && has("ptah_get_persistent_agent") {
        capabilities.push(CapabilityDescriptor {
            id: "agent.continuity".into(),
            tier: CapabilityTier::Observe,
            mutating: false,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "Observe durable agent identity and continuation state.".into(),
        });
    }
    if has("ptah_resume_persistent_agent") {
        capabilities.push(CapabilityDescriptor {
            id: "agent.resume".into(),
            tier: CapabilityTier::Execute,
            mutating: true,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "Resume a persistent agent with an explicit fresh prompt.".into(),
        });
    }
    if has("ptah_list_computer_runs")
        && has("ptah_get_computer_run")
        && has("ptah_get_computer_run_events")
    {
        capabilities.push(CapabilityDescriptor {
            id: "computer.observe".into(),
            tier: CapabilityTier::ComputerObserve,
            mutating: false,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "Read redacted, scope-bound Computer Use projections and audit events."
                .into(),
        });
    }
    // `computer.control` is advertised only when the *whole* gated sequence is
    // reachable. Advertising control without the approval request/read pair
    // would describe a human gate a consumer has no way to satisfy.
    if has("ptah_request_computer_approval")
        && has("ptah_get_computer_approval")
        && has("ptah_authorize_computer_run")
        && has("ptah_pause_computer_run")
        && has("ptah_take_over_computer_run")
        && has("ptah_cancel_computer_run")
    {
        capabilities.push(CapabilityDescriptor {
            id: "computer.control".into(),
            tier: CapabilityTier::ComputerControl,
            mutating: true,
            human_gate: true,
            availability: CapabilityAvailability::Gated,
            description: "Spend a host-issued human approval receipt to take lease- and \
                revision-fenced semantic Computer Use control."
                .into(),
        });
    }

    CapabilitySet {
        contract: CAPABILITY_CONTRACT_VERSION.into(),
        capabilities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_set_is_forward_safe_and_marks_high_risk_controls_gated() {
        let set = advertised_capabilities();
        assert_eq!(set.contract, CAPABILITY_CONTRACT_VERSION);
        assert!(set.get("session.observe").is_some());
        assert_eq!(
            set.get("run.promote")
                .map(|capability| capability.availability),
            Some(CapabilityAvailability::Gated)
        );
        assert_eq!(
            set.get("computer.control")
                .map(|capability| capability.human_gate),
            Some(true)
        );
    }

    #[test]
    fn capability_revision_tracks_authority_not_prose() {
        let baseline = capability_revision();
        assert_eq!(baseline.len(), 64);
        assert!(baseline.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(baseline, capability_revision_of(&advertised_capabilities()));

        let mut reworded = advertised_capabilities();
        for capability in &mut reworded.capabilities {
            capability.description = "reworded".into();
        }
        assert_eq!(
            capability_revision_of(&reworded),
            baseline,
            "prose churn must not invalidate outstanding human approvals"
        );

        let mut ungated = advertised_capabilities();
        for capability in &mut ungated.capabilities {
            if capability.id == "computer.control" {
                capability.human_gate = false;
                capability.availability = CapabilityAvailability::Available;
            }
        }
        assert_ne!(
            capability_revision_of(&ungated),
            baseline,
            "dropping the computer.control human gate must change the revision"
        );

        let mut narrowed = advertised_capabilities();
        narrowed
            .capabilities
            .retain(|capability| capability.id != "computer.control");
        assert_ne!(capability_revision_of(&narrowed), baseline);
    }

    #[test]
    fn capability_payload_is_json_round_trippable() {
        let set = advertised_capabilities();
        let encoded = serde_json::to_vec(&set).expect("capability payload serializes");
        let decoded: CapabilitySet =
            serde_json::from_slice(&encoded).expect("capability payload deserializes");
        assert_eq!(decoded, set);
    }
}
