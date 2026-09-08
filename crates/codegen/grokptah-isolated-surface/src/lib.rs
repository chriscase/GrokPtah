//! Isolated Surface Proof Harness v0 (#288/#286).
//!
//! Synthetic contract + host-sentinel machinery for the Sep 18 physical
//! Virtualization.framework proof. This crate does **not** claim packaged VM
//! qualification from Linux CI or simulator evidence alone.

mod backend;
mod captured_frame;
mod channels;
mod checklist_runner;
mod contained_browser;
mod contained_browser_dry_run;
mod error;
mod evidence_pack;
mod harness;
mod lifecycle;
mod mac_host_sentinel;
#[cfg(any(target_os = "macos", test))]
mod main_checkout_fence;
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
pub use captured_frame::{
    admit_browser_engine_capture, admit_captured_frame_with_claimed_digest,
    assert_postcondition_change, canonical_sha256_digest, is_canonical_sha256_digest,
    require_frame_for_postcondition, simulator_synthetic_frame_bytes, BoundedCapturedFrame,
    CapturedFrameEvidence, CapturedFrameMediaKind, CapturedFramePair, CapturedFrameSource,
    MAX_CAPTURED_FRAME_BYTES, SYNTHETIC_FRAME_HEIGHT, SYNTHETIC_FRAME_PAYLOAD_NEEDLE,
    SYNTHETIC_FRAME_WIDTH,
};
pub use channels::ChannelRegistry;
pub use checklist_runner::{
    run_sep18_checklist, Sep18ChecklistRunOutcome, Sep18ChecklistRunnerConfig,
};
pub use contained_browser::ContainedBrowserBackend;
pub use contained_browser_dry_run::{
    run_contained_browser_fault_matrix, run_contained_browser_stop_fence_regression,
    ContainedBrowserDryRunEvidence, ContainedBrowserDryRunOutcome, ContainedBrowserDryRunPlatform,
};
pub use error::{HarnessError, HarnessErrorCode, HarnessResult};
pub use evidence_pack::{
    parse_evidence_pack, seal_contained_browser_dry_run_pack, seal_synthetic_harness_pack,
    seal_vf_dry_run_pack, serialize_evidence_pack, verifier_exit_code, verify_evidence_pack,
    EvidenceVerifierCode, EvidenceVerifierDecision, HostSentinelProbeSummary, PhysicalProofMarkers,
    Sep18ChecklistSubstrate, Sep18EvidencePack, EVIDENCE_PACK_SCHEMA_VERSION,
};
pub use harness::{IsolatedSurfaceHarness, StopEvidence};
pub use lifecycle::{
    GuestLifecycle, GuestLifecycleDisposition, GuestLifecyclePhase, ProofEvidenceClass,
    LIFECYCLE_SCHEMA_VERSION,
};
pub use mac_host_sentinel::{mac_host_sentinel_platform_support, MacHostSentinelPlatformSupport};
pub use proof_sequencer::{
    ChecklistStep, FaultMatrixCase, SealedProofEvidence, Sep18NoModelProofSequencer,
};
#[cfg(target_os = "macos")]
pub use sentinel::MacHostSentinelCollector;
pub use sentinel::{
    HostSentinelDiff, HostSentinelProbe, HostSentinelProbeKind, HostSentinelRegistry,
    HostSentinelSnapshot, MainCheckoutFence, SyntheticHostProbe,
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

/// Non-claim for Contained Browser dry-run artifacts. Substrate rehearsal only.
pub const CONTAINED_BROWSER_DRY_RUN_NONCLAIM: &str =
    "Contained Browser substrate v0 is not isolation PASS; never claim Virtualization.framework PASS or enable admission.";
