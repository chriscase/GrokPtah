//! Provider-neutral Computer Use contracts and simulator (#267/#268).
//!
//! The safety kernel remains provider-neutral. Native adapters must implement
//! [`ComputerBackend`] and pass through the policy/state layer. The first macOS
//! adapter exposes local consented observation and bounded semantic actions.
//! Model proposals live above this module so provider behavior cannot weaken
//! the state machine or policy boundary; MCP mutations remain absent.
//!
//! [`projection`] derives the redaction-safe serialized view that the desktop
//! cockpit and any future coordinator surface both consume, so the two cannot
//! disagree about run state, control disposition, epoch, or event range.

mod macos_observation;
mod platform;
mod policy;
mod projection;
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
    project_run_at, ActionGrantSummary, ComputerRunCapacity, ComputerRunEventPage,
    ComputerRunEventRange, ComputerRunProgress, ComputerRunProjection, ComputerTargetSummary,
    ObservationSummary, DEFAULT_EVENT_PAGE, MAX_EVENT_PAGE,
};

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
