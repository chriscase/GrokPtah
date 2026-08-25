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
/// The advertised set for a host, including external workers.
///
/// External-worker availability cannot be derived from `CONTROL_TOOLS` the way
/// every other capability is: the tools are always compiled in, but a provider
/// is only usable once bootstrap installs a qualified adapter. Availability
/// therefore comes from the live registry, so an absent or unqualified provider
/// advertises `Unavailable` and fails closed rather than being discovered at
/// call time.
pub fn advertised_capabilities_for(
    registry: &crate::external_worker::ExternalWorkerRegistry,
) -> CapabilitySet {
    let mut set = advertised_capabilities();
    set.capabilities.push(external_worker_capability(registry));
    set
}

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

/// Advertise the external-worker capability for a given provider registry.
///
/// Availability is derived from the registry rather than declared, because a
/// capability that claims to exist when no qualified adapter is installed is a
/// promise the host cannot keep. `Unavailable` is the honest answer until
/// bootstrap installs an adapter and its repository allowlist; `Gated` is the
/// answer afterwards, because the authority record — not the presence of the
/// adapter — decides every individual action.
pub fn external_worker_capability(
    registry: &crate::external_worker::ExternalWorkerRegistry,
) -> CapabilityDescriptor {
    let installed = registry.providers();
    CapabilityDescriptor {
        id: "external.worker".into(),
        tier: CapabilityTier::Execute,
        mutating: true,
        // Launching work into a repository on a third-party provider is not
        // something a caller should be able to reach without an explicit grant.
        human_gate: true,
        availability: if installed.is_empty() {
            CapabilityAvailability::Unavailable
        } else {
            CapabilityAvailability::Gated
        },
        description: "Launch and steer isolated provider-managed coding workers under a durable authority record.".into(),
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

    /// A capability that claims to exist with no adapter installed is a
    /// promise the host cannot keep, so availability is read from the registry.
    #[test]
    fn external_worker_capability_reflects_what_is_actually_installed() {
        use crate::external_worker::{CursorCloudAdapter, ExternalWorkerRegistry};
        use std::sync::Arc;

        let registry = ExternalWorkerRegistry::new();
        let empty = external_worker_capability(&registry);
        assert_eq!(empty.id, "external.worker");
        assert_eq!(empty.availability, CapabilityAvailability::Unavailable);

        registry.register(Arc::new(CursorCloudAdapter::new("synthetic-key").unwrap()));
        let installed = external_worker_capability(&registry);
        // Installed is still only `Gated`: the durable authority record, not
        // the presence of an adapter, decides each individual action.
        assert_eq!(installed.availability, CapabilityAvailability::Gated);
        assert!(installed.human_gate);
        assert!(installed.mutating);
    }

    /// The advertised set must tell the truth about what is installed, because
    /// a consumer negotiates against it before calling anything.
    #[test]
    fn the_advertised_set_reports_external_workers_from_the_live_registry() {
        use crate::external_worker::{CursorCloudAdapter, ExternalWorkerRegistry};
        use std::sync::Arc;

        let registry = ExternalWorkerRegistry::new();
        let set = advertised_capabilities_for(&registry);
        assert_eq!(
            set.get("external.worker").map(|item| item.availability),
            Some(CapabilityAvailability::Unavailable),
        );

        registry.register(Arc::new(CursorCloudAdapter::new("synthetic-key").unwrap()));
        let set = advertised_capabilities_for(&registry);
        assert_eq!(
            set.get("external.worker").map(|item| item.availability),
            Some(CapabilityAvailability::Gated),
        );
        // The rest of the advertised set is unchanged by the registry.
        assert!(set.get("session.observe").is_some());
        let encoded = serde_json::to_vec(&set).expect("payload serializes");
        let decoded: CapabilitySet = serde_json::from_slice(&encoded).expect("round trips");
        assert_eq!(decoded, set);
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
