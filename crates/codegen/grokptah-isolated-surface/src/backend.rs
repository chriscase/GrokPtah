//! Isolated surface backend SPI.
//!
//! Harness orchestrates lifecycle, channels, and host sentinels. Backends
//! implement guest boot, frame observation, guest-local inject, and teardown
//! only — never host pointer/keyboard/clipboard paths.

use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::simulator::{GuestFrame, GuestLocalAction, InjectOutcome};

/// Backend SPI the harness drives for isolated guest surfaces.
pub trait IsolatedSurfaceBackend {
    fn evidence_class(&self) -> ProofEvidenceClass;

    fn boot(&mut self) -> HarnessResult<GuestFrame>;

    fn observe_frame(&self) -> HarnessResult<GuestFrame>;

    fn inject_guest_local(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome>;

    /// Fence the backend dispatch mechanism before teardown (Stop-path).
    ///
    /// Production adapters must implement this explicitly: fence guest-local inject
    /// (or equivalent dispatch halt) and return `Ok(())` only on acknowledged fence
    /// success. Unimplemented or unavailable fences must return
    /// [`HarnessError::backend_unavailable`] (or another explicit error) — never
    /// inherit a silent success default.
    fn stop_fence_first(&mut self) -> HarnessResult<()>;

    fn destroy(&mut self) -> HarnessResult<()>;

    fn is_booted(&self) -> bool;
}

/// Seal an evidence class label — backends must not upgrade labels at runtime.
pub fn assert_evidence_class_unchanged(
    declared: ProofEvidenceClass,
    observed: ProofEvidenceClass,
) -> HarnessResult<()> {
    if declared != observed {
        return Err(HarnessError::invalid_state(format!(
            "evidence class label must not be upgraded: declared {:?}, observed {:?}",
            declared, observed
        )));
    }
    Ok(())
}

/// Receipt attesting a physical Mac VF launch for Sep 18 proof gate only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfLaunchReceipt {
    pub physical_mac_proof_id: String,
}

/// Evidence class permitted at harness construction without a VF receipt.
pub fn honest_harness_evidence_class(
    backend_class: ProofEvidenceClass,
) -> HarnessResult<ProofEvidenceClass> {
    match backend_class {
        ProofEvidenceClass::VirtualizationFramework => Err(HarnessError::invalid_state(
            "VirtualizationFramework evidence class requires a physical VF launch receipt; use with_vf_backend",
        )),
        other => Ok(other),
    }
}
