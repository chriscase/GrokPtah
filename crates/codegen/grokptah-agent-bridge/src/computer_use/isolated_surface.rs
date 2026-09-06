//! Fail-closed bridge seam for the Isolated Surface Proof Harness (#288/#286).
//!
//! The semantic macOS Computer Run path remains unchanged. This module exposes
//! the synthetic harness and an admission gate that stays false until a native
//! Virtualization.framework adapter passes the Sep 18 physical proof checklist.

#[cfg(all(target_os = "macos", feature = "vf-backend"))]
pub use grokptah_isolated_surface::VirtualizationFrameworkBackend;
pub use grokptah_isolated_surface::{
    assert_evidence_class_unchanged, honest_harness_evidence_class,
    isolated_surface_admission_available, ChannelRegistry, ChecklistStep, ContainedBrowserBackend,
    ContainedBrowserDryRunEvidence, ContainedBrowserDryRunOutcome, ContainedBrowserDryRunPlatform,
    FaultInjectingBackend, FaultMatrixCase, FrameDelta, GuestFrame, GuestLifecycle,
    GuestLifecycleDisposition, GuestLifecyclePhase, GuestLocalAction, HarnessError,
    HarnessErrorCode, HarnessResult, HostSentinelDiff, HostSentinelProbe, HostSentinelRegistry,
    HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness, MainCheckoutFence,
    ProofEvidenceClass, SealedProofEvidence, Sep18NoModelProofSequencer, StopEvidence,
    SyntheticGuest, SyntheticGuestAction, SyntheticHostProbe, VfDryRunEvidence, VfDryRunOutcome,
    VfDryRunPlatform, VfLaunchReceipt, CONTAINED_BROWSER_DRY_RUN_NONCLAIM,
    SYNTHETIC_HARNESS_NONCLAIM, VF_DRY_RUN_NONCLAIM,
};

/// Bridge-level admission check. Remains unavailable until physical Mac proof.
pub fn computer_use_isolated_surface_admission() -> bool {
    isolated_surface_admission_available()
}
