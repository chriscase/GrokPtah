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

mod macos_observation;
mod platform;
mod policy;
mod projection;
mod reads;
mod service;
mod simulator;
mod store;
mod types;

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
    ComputerCapabilities, ComputerCapabilityProof, ComputerCapabilityTier,
    ComputerControlDisposition, ComputerError, ComputerErrorCode, ComputerKey, ComputerObservation,
    ComputerPrincipal, ComputerRun, ComputerRunState, ComputerSurfaceBinding, ComputerTarget,
    ComputerUseLimits, EvidenceRef, GrantIssuer, IsolationProofOrigin, ObservationAuthority,
    ObservationGeometry, PointerButton, SemanticAction, SemanticElement, Sensitivity,
    SurfaceFreshnessFence, MACOS_INTERRUPTED_BACKEND_ID, MACOS_NATIVE_BACKEND_ID,
    SIMULATOR_BACKGROUND_BACKEND_ID, SIMULATOR_FOREGROUND_BACKEND_ID,
    SIMULATOR_ISOLATED_BACKEND_ID,
};
