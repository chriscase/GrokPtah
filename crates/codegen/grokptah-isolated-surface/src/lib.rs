//! Isolated Surface Proof Harness v0 (#288/#286).
//!
//! Synthetic contract + host-sentinel machinery for the Sep 18 physical
//! Virtualization.framework proof. This crate does **not** claim packaged VM
//! qualification from Linux CI or simulator evidence alone.

mod backend;
mod channels;
mod contained_browser;
mod error;
mod harness;
mod lifecycle;
mod proof_sequencer;
mod sentinel;
mod simulator;
mod store;
#[cfg(all(target_os = "macos", feature = "vf-backend"))]
mod vf_backend;
mod vf_dry_run;

pub use backend::{
    assert_evidence_class_unchanged, honest_harness_evidence_class, IsolatedSurfaceBackend,
    VfLaunchReceipt,
};
pub use channels::ChannelRegistry;
pub use contained_browser::ContainedBrowserBackend;
pub use error::{HarnessError, HarnessErrorCode, HarnessResult};
pub use harness::{IsolatedSurfaceHarness, StopEvidence};
pub use lifecycle::{
    GuestLifecycle, GuestLifecycleDisposition, GuestLifecyclePhase, ProofEvidenceClass,
    LIFECYCLE_SCHEMA_VERSION,
};
pub use proof_sequencer::{
    ChecklistStep, FaultMatrixCase, SealedProofEvidence, Sep18NoModelProofSequencer,
};
#[cfg(target_os = "macos")]
pub use sentinel::MacHostSentinelCollector;
pub use sentinel::{
    HostSentinelDiff, HostSentinelProbe, HostSentinelRegistry, HostSentinelSnapshot,
    MainCheckoutFence, SyntheticHostProbe,
};
pub use simulator::{
    FaultInjectingBackend, FrameDelta, GuestFrame, GuestLocalAction, InjectOutcome, SyntheticGuest,
    SyntheticGuestAction,
};
pub use store::{snapshot_root, HarnessSnapshot, SNAPSHOT_FILE, SNAPSHOT_SCHEMA_VERSION};
#[cfg(all(target_os = "macos", feature = "vf-backend"))]
pub use vf_backend::VirtualizationFrameworkBackend;
pub use vf_dry_run::{VfDryRunEvidence, VfDryRunOutcome, VfDryRunPlatform};

/// Fail-closed admission gate for bridge integration. Remains false until a
/// native adapter passes the physical Mac proof checklist.
pub fn isolated_surface_admission_available() -> bool {
    false
}

/// Human-readable non-claim for proof artifacts.
pub const SYNTHETIC_HARNESS_NONCLAIM: &str =
    "Synthetic harness evidence is ineligible for Virtualization.framework qualification.";

/// Non-claim for VF dry-run artifacts. Does not assert physical Sep 18 PASS.
pub const VF_DRY_RUN_NONCLAIM: &str =
    "VF dry-run is not physical Mac proof; Linux CI and dry-run artifacts never claim Sep 18 PASS.";
