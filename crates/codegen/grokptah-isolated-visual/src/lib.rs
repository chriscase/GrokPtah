//! Host-owned isolated Computer Use guest/VM lifecycle and packaged authority.
//!
//! This crate holds one authority over isolated Computer Use:
//!
//! - hermetic content-addressed source/image resolution
//! - a finite guest lifecycle `create → ready → running → closing`, with
//!   terminal truth kept separate from phase
//! - host-issued surface leases, conflict domains, and exactly-once dispatch
//! - bounded frame/input transport and a redacted public projection
//! - packaged admission driven by an OS code-signing probe and an operator
//!   trust root, with a deterministic simulator for the parts that can be
//!   exercised without hardware
//!
//! # What this crate does not claim
//!
//! Simulator evidence and source compilation are **not** VM qualification, and
//! the type system says so: [`IsolatedEvidenceClass`] only reaches
//! `VirtualizationFramework` through [`IsolatedPreflight::with_observed_launch`],
//! which refuses unless launch intent was admitted from real signed artifacts.
//! Nothing here performs global mouse/keyboard injection, CGEvent fallback,
//! AppleScript, clipboard access, or credential UI.

pub mod cleanup;
pub mod clock;
pub mod code_identity;
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
pub mod trust_root;

pub use cleanup::{
    CleanupOutcome, CleanupProbe, CleanupReceipt, IsolatedCleanupReason, ResourceProbeResult,
    ResourceState, REQUIRED_RESOURCES,
};
pub use clock::{HostClock, SystemClock, TestClock};
pub use code_identity::{
    CapturedCodesignOutput, CodeIdentityProbe, ObservedCodeIdentity, SigningClass,
    SystemCodeIdentityProbe,
};
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
    hash_file, inspect_guest_image, inspect_helper_bundle, parse_semver, versions_compatible,
    AdmittedGuestImage, AdmittedHelperIdentity, GuestImageObservation, PackagedHelperObservation,
    APP_BUNDLE_ID, APP_EXECUTABLE, APP_MINIMUM_OS, APP_PRODUCT_NAME, APP_VERSION,
    COMPUTER_USE_MINIMUM_OS, DEMO_TARGET_BUNDLE_ID, HELPER_BUNDLE_ID, HELPER_EXECUTABLE,
    HELPER_MINIMUM_OS, HELPER_NESTED_PATH, HELPER_PRODUCT_NAME, HELPER_VERSION,
    PACKAGE_IDENTITY_SCHEMA, SELF_ATTESTATION_FILENAMES,
};
pub use preflight::{DenyReason, IsolatedPreflight, ARTIFACT_ROOT_ENV};
pub use projection::IsolatedVisualProjection;
pub use resolver::{ContentAddressedStore, HermeticResolver};
pub use store::{IsolatedVisualStore, RecoveryReport};
pub use trust_root::{
    AppTrustAnchor, GuestImageTrustAnchor, HelperTrustAnchor, PackagedTrustRoot, TRUST_ROOT_ENV,
    TRUST_ROOT_SCHEMA,
};

#[cfg(test)]
mod host_tests;
