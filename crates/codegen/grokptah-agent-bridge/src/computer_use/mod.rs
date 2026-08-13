//! Provider-neutral Computer Use contracts and simulator (#267/#268).
//!
//! The safety kernel remains provider-neutral. Native adapters must implement
//! [`ComputerBackend`] and pass through the policy/state layer. The first macOS
//! adapter exposes local read-only observation; model, action, and MCP surfaces
//! remain deliberately absent.

mod macos_observation;
mod platform;
mod policy;
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

#[cfg(target_os = "macos")]
mod macos_native;
pub use service::ComputerUseService;
pub use simulator::SimulatorBackend;
pub use store::ComputerStore;
pub use types::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerAuditEntry, ComputerBackend,
    ComputerCapabilities, ComputerError, ComputerErrorCode, ComputerObservation, ComputerRun,
    ComputerRunState, ComputerTarget, ComputerUseLimits, EvidenceRef, GrantIssuer,
    ObservationGeometry, PointerButton, SemanticAction, SemanticElement, Sensitivity,
};
