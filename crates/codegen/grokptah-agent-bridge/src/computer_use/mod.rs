//! Provider-neutral Computer Use contracts and simulator (#267/#268).
//!
//! The safety kernel remains provider-neutral. Native adapters must implement
//! [`ComputerBackend`] and pass through the policy/state layer. The first macOS
//! adapter exposes local consented observation and bounded semantic actions.
//! Model proposals live above this module so provider behavior cannot weaken
//! the state machine or policy boundary; MCP mutations remain absent.
//!
//! [`projection`] derives the redaction-safe serialized view that the desktop
//! cockpit and any coordinator surface both consume, so the two cannot
//! disagree about run state, control disposition, epoch, or event range.
//! Which runs each surface may list is a separate gate: the cockpit is
//! session-scoped; coordinator reads take [`ComputerReadBinding`].

mod control;
// Isolated visual substrate. Every module here is private to `computer_use`:
// nothing below is re-exported from this module or from the crate root, and
// no dispatch path reaches it. It stays that way until the measured
// signed-hardware gates and the independent security/accessibility reviews
// pass.
mod isolated_guest;
mod isolated_visual;
mod isolated_visual_artifacts;
mod isolated_visual_channel;
mod isolated_visual_driver;
mod isolated_visual_frames;
mod isolated_visual_helper;
mod isolated_visual_helper_control;
mod isolated_visual_input;
mod isolated_visual_input_wire;
#[cfg(test)]
mod isolated_visual_package;
mod isolated_visual_protocol;
mod isolated_visual_runtime;
mod isolated_visual_selfcheck;
mod isolated_visual_status;
mod isolated_visual_stream;
mod macos_observation;
mod platform;
mod policy;
mod projection;
mod reads;
mod service;
mod simulator;
mod store;
mod types;

pub use control::{
    ComputerAgentObservation, ComputerClientIdentity, ComputerGrantRequest,
    ComputerRunAgentController, ComputerRunController,
};
pub use isolated_visual_status::{
    computer_isolated_visual_status, ComputerIsolatedVisualBlocker, ComputerIsolatedVisualStatus,
};
pub use macos_observation::MacOsObservationPlatform;
pub use platform::{
    ComputerObservationPlatform, ComputerPermission, ComputerPermissionStatus,
    ComputerPlatformStatus, ComputerTargetCandidate,
};
pub use policy::ComputerPolicy;
pub use projection::{
    project_run_at, ActionGrantSummary, ActionOutcomeSummary, ComputerErrorSummary,
    ComputerRunCapacity, ComputerRunEventPage, ComputerRunEventRange, ComputerRunProgress,
    ComputerRunProjection, ComputerScopeCapacity, ComputerTargetSummary, ObservationSummary,
    DEFAULT_EVENT_PAGE, MAX_EVENT_PAGE,
};
pub use reads::{ComputerReadBinding, ComputerRunReads};

/// Canonical string form of a workspace path for the durable Computer Run
/// binding. This is the same canonicalization the control plane applies to a
/// caller-claimed workspace, so binding equality is an exact string compare.
pub fn canonical_workspace_string(path: &std::path::Path) -> Option<String> {
    crate::orchestration::canonical_workspace(path)
        .ok()
        .map(|canonical| canonical.display().to_string())
}

#[cfg(target_os = "macos")]
mod macos_native;
pub use service::ComputerUseService;
pub use simulator::SimulatorBackend;
pub use store::ComputerStore;
pub use types::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerAuditEntry, ComputerBackend,
    ComputerCapabilities, ComputerControlDisposition, ComputerError, ComputerErrorCode,
    ComputerObservation, ComputerRun, ComputerRunState, ComputerTarget, ComputerUseLimits,
    EvidenceRef, GrantIssuer, ObservationGeometry, PointerButton, SemanticAction, SemanticElement,
    Sensitivity,
};
