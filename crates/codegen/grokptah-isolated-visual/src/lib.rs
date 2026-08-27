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
pub mod occupancy;
pub mod packaged_authority;
pub mod preflight;
pub mod projection;
pub mod protocol;
pub mod resolver;
pub mod simulator;
pub mod store;

pub use cleanup::{
    ChannelRevocationObservation, HelperExitObservation, IsolatedCleanupEvidence,
    IsolatedCleanupObservation, IsolatedCleanupReason, OccupancyReleaseObservation,
    OverlayRemovalObservation, VmStopObservation,
};
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
pub use occupancy::{resource_key, OccupancyRecord, OccupancyState, OccupancyStore};
pub use packaged_authority::{
    admit_guest_image, admit_packaged_helper, documented_identity_json, hash_bundle_manifest,
    hash_file, inspect_artifact_root, inspect_codesign_fields, parse_semver, versions_compatible,
    write_admitted_fixture, ExpectedGuestImage, ExpectedHelper, GuestImageObservation,
    PackagedHelperObservation, SigningClass, APP_BUNDLE_ID, APP_EXECUTABLE, APP_MINIMUM_OS,
    APP_PRODUCT_NAME, APP_VERSION, COMPUTER_USE_MINIMUM_OS, DEMO_TARGET_BUNDLE_ID,
    HELPER_BUNDLE_ID, HELPER_EXECUTABLE, HELPER_MINIMUM_OS, HELPER_NESTED_PATH,
    HELPER_PRODUCT_NAME, HELPER_VERSION, PACKAGE_IDENTITY_SCHEMA,
};
pub use preflight::IsolatedPreflight;
pub use projection::IsolatedVisualProjection;
pub use resolver::{ContentAddressedStore, HermeticResolver};
pub use store::IsolatedVisualStore;

#[cfg(test)]
mod host_tests;
