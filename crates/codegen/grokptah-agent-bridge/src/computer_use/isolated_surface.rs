//! Fail-closed bridge seam for the Isolated Surface Proof Harness (#288/#286).
//!
//! The semantic macOS Computer Run path remains unchanged. This module exposes
//! the synthetic harness and an admission gate that stays false until a native
//! Virtualization.framework adapter passes the Sep 18 physical proof checklist.

pub use grokptah_isolated_surface::{
    assert_evidence_class_unchanged, isolated_surface_admission_available, ChannelRegistry,
    ChecklistStep, ContainedBrowserBackend, FaultMatrixCase, FrameDelta, GuestFrame,
    GuestLifecycle, GuestLifecycleDisposition, GuestLifecyclePhase, GuestLocalAction, HarnessError,
    HarnessErrorCode, HarnessResult, HostSentinelDiff, HostSentinelProbe, HostSentinelRegistry,
    HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness, MainCheckoutFence,
    ProofEvidenceClass, SealedProofEvidence, Sep18NoModelProofSequencer, StopEvidence,
    SyntheticGuest, SyntheticGuestAction, SyntheticHostProbe, SYNTHETIC_HARNESS_NONCLAIM,
};

/// Bridge-level admission check. Remains unavailable until physical Mac proof.
pub fn computer_use_isolated_surface_admission() -> bool {
    isolated_surface_admission_available()
}
