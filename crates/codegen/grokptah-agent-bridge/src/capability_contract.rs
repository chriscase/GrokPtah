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

use crate::orchestration::CONTROL_TOOLS;

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
    if has("ptah_list_work_graphs")
        && has("ptah_get_work_graph")
        && has("ptah_get_work_graph_evidence")
    {
        capabilities.push(CapabilityDescriptor {
            id: "work.graph.observe".into(),
            tier: CapabilityTier::Observe,
            mutating: false,
            human_gate: false,
            availability: CapabilityAvailability::Available,
            description: "Read secret-free durable work-graph status, leases, and evidence.".into(),
        });
    }
    if has("ptah_cancel_work_graph") && has("ptah_cancel_work_item") && has("ptah_review_work_item")
    {
        capabilities.push(CapabilityDescriptor {
            id: "work.graph.control".into(),
            tier: CapabilityTier::Review,
            mutating: true,
            // Discarding a reviewed result is destructive and stays gated.
            human_gate: true,
            availability: CapabilityAvailability::Gated,
            description: "Cancel work-graph items or graphs and keep or discard reviewed results."
                .into(),
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
    if has("ptah_authorize_computer_run")
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
            description: "Use lease- and revision-fenced semantic Computer Use controls.".into(),
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
    fn capability_payload_is_json_round_trippable() {
        let set = advertised_capabilities();
        let encoded = serde_json::to_vec(&set).expect("capability payload serializes");
        let decoded: CapabilitySet =
            serde_json::from_slice(&encoded).expect("capability payload deserializes");
        assert_eq!(decoded, set);
    }
}
