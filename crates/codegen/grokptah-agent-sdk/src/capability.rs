//! Client-side projection of `tools/list`. This is not a host capability document.

use std::collections::BTreeSet;

/// Discovery state for one SDK capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapabilityState {
    /// The backing MCP tool is present on `tools/list`.
    Available,
    /// The backing MCP tool is absent. Calling the method returns `unsupported`.
    Unavailable,
    /// Permanently refused by this SDK. Never called.
    Forbidden,
}

/// Read-only capability projection. Computer control and provider credentials
/// are always [`CapabilityState::Forbidden`]; they are not inferred from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub session_list: CapabilityState,
    pub run_observe: CapabilityState,
    pub run_events_page: CapabilityState,
    pub host_capacity: CapabilityState,
    pub computer_control: CapabilityState,
    pub provider_credentials: CapabilityState,
}

pub(crate) const TOOL_LIST_SESSIONS: &str = "ptah_list_sessions";
pub(crate) const TOOL_LIST_RUNS: &str = "ptah_list_runs";
pub(crate) const TOOL_GET_RUN: &str = "ptah_get_run";
pub(crate) const TOOL_GET_EVENTS: &str = "ptah_get_events";
pub(crate) const TOOL_GET_CAPACITY: &str = "ptah_get_capacity";

impl Capabilities {
    pub(crate) fn from_tool_names(names: &BTreeSet<String>) -> Self {
        Self {
            session_list: present(names, TOOL_LIST_SESSIONS),
            run_observe: present(names, TOOL_GET_RUN),
            run_events_page: present(names, TOOL_GET_EVENTS),
            host_capacity: present(names, TOOL_GET_CAPACITY),
            computer_control: CapabilityState::Forbidden,
            provider_credentials: CapabilityState::Forbidden,
        }
    }
}

fn present(names: &BTreeSet<String>, tool: &str) -> CapabilityState {
    if names.contains(tool) {
        CapabilityState::Available
    } else {
        CapabilityState::Unavailable
    }
}
