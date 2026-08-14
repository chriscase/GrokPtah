//! Provider-neutral Computer Use contracts and simulator (#267/#268).
//!
//! This module intentionally contains no native capture/input implementation,
//! model tool registration, Tauri command, or MCP surface. Platform adapters
//! must implement [`ComputerBackend`] and pass through the policy/state layer.

mod policy;
mod service;
mod simulator;
mod store;
mod types;

pub use policy::ComputerPolicy;
pub use service::ComputerUseService;
pub use simulator::SimulatorBackend;
pub use store::ComputerStore;
pub use types::{
    ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerAuditEntry, ComputerBackend,
    ComputerCapabilities, ComputerError, ComputerErrorCode, ComputerObservation, ComputerRun,
    ComputerRunState, ComputerTarget, ComputerUseLimits, EvidenceRef, GrantIssuer,
    ObservationGeometry, PointerButton, SemanticAction, SemanticElement, Sensitivity,
};
