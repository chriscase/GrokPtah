//! Isolated Surface Proof Harness v0 (#288/#286).
//!
//! Synthetic contract + host-sentinel machinery for the Sep 18 physical
//! Virtualization.framework proof. This crate does **not** claim packaged VM
//! qualification from Linux CI or simulator evidence alone.

mod channels;
mod error;
mod harness;
mod lifecycle;
mod sentinel;
mod simulator;
mod store;

pub use channels::ChannelRegistry;
pub use error::{HarnessError, HarnessErrorCode, HarnessResult};
pub use harness::{IsolatedSurfaceHarness, StopEvidence};
pub use lifecycle::{
    GuestLifecycle, GuestLifecycleDisposition, GuestLifecyclePhase, ProofEvidenceClass,
    LIFECYCLE_SCHEMA_VERSION,
};
pub use sentinel::{
    HostSentinelDiff, HostSentinelProbe, HostSentinelRegistry, HostSentinelSnapshot,
    SyntheticHostProbe,
};
pub use simulator::{FrameDelta, GuestFrame, SyntheticGuest, SyntheticGuestAction};
pub use store::{snapshot_root, HarnessSnapshot, SNAPSHOT_FILE, SNAPSHOT_SCHEMA_VERSION};

/// Fail-closed admission gate for bridge integration. Remains false until a
/// native adapter passes the physical Mac proof checklist.
pub fn isolated_surface_admission_available() -> bool {
    false
}

/// Human-readable non-claim for proof artifacts.
pub const SYNTHETIC_HARNESS_NONCLAIM: &str =
    "Synthetic harness evidence is ineligible for Virtualization.framework qualification.";
