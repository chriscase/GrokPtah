//! Host-owned isolated Computer Use guest/VM lifecycle.
//!
//! This crate reconstructs the #288 isolated visual backend as a production-shaped
//! authority model that converges on one Computer Run and one Work/Attempt:
//!
//! - hermetic content-addressed source/image resolver
//! - finite guest lifecycle `create → ready → running → closing`
//! - host-issued surface leases, conflict domains, and exactly-once dispatch
//! - bounded frame/input transport and redacted public projection
//! - deterministic simulator plus fail-closed Virtualization.framework preflight
//!
//! Simulator evidence and source compilation are ineligible for VM qualification.
//! This crate never performs global mouse/keyboard injection, CGEvent fallback,
//! AppleScript, clipboard access, or credential UI.

pub mod cleanup;
pub mod clock;
pub mod error;
pub mod git_hermetic;
pub mod host;
pub mod ids;
pub mod lease;
pub mod lifecycle;
pub mod manifest;
pub mod preflight;
pub mod projection;
pub mod protocol;
pub mod resolver;
pub mod simulator;
pub mod store;

pub use cleanup::{IsolatedCleanupEvidence, IsolatedCleanupReason};
pub use clock::{HostClock, SystemClock, TestClock};
pub use error::{IsolatedError, IsolatedErrorCode, IsolatedResult};
pub use host::{CreateGuestRequest, IsolatedVisualHost};
pub use ids::{LIVE_DESKTOP_CONFLICT_DOMAIN, SCHEMA_VERSION};
pub use lease::{
    ComputerDispatchRecord, ComputerDispatchState, ComputerSurfaceLease, ComputerSurfaceLeaseState,
};
pub use lifecycle::{
    IsolatedEvidenceClass, IsolatedGuestPhase, IsolatedGuestRecord, IsolatedGuestTerminal,
};
pub use manifest::{
    ComputerSurfaceBinding, HelperIdentity, IsolatedSourceManifest, IsolatedVisualManifest,
    IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile,
};
pub use preflight::IsolatedPreflight;
pub use projection::IsolatedVisualProjection;
pub use resolver::{ContentAddressedStore, HermeticResolver};
pub use store::IsolatedVisualStore;

#[cfg(test)]
mod host_tests;
