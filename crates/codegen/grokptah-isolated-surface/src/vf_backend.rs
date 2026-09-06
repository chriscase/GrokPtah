//! Virtualization.framework backend — Mac-only, feature-gated dry-run stub.
//!
//! Implements [`IsolatedSurfaceBackend`] with an honest
//! `ProofEvidenceClass::VirtualizationFramework` label. Every operation fails closed
//! until a physical Mac worker completes the Sep 18 checklist with a signed guest
//! image and live VF IPC. Attach only via [`IsolatedSurfaceHarness::with_vf_backend`].

use crate::backend::IsolatedSurfaceBackend;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::simulator::{GuestFrame, GuestLocalAction, InjectOutcome};

const DRY_RUN_MESSAGE: &str = "Virtualization.framework backend is dry-run only; physical Mac Sep 18 proof required before boot";

/// Mac-facing Virtualization.framework backend stub. Requires the `vf-backend` cargo
/// feature and must be wired through [`IsolatedSurfaceHarness::with_vf_backend`].
#[derive(Debug, Clone, Default)]
pub struct VirtualizationFrameworkBackend {
    fenced: bool,
    boot_attempted: bool,
}

impl VirtualizationFrameworkBackend {
    pub fn new() -> Self {
        Self {
            fenced: false,
            boot_attempted: false,
        }
    }

    pub fn is_fenced(&self) -> bool {
        self.fenced
    }

    pub fn boot_attempted(&self) -> bool {
        self.boot_attempted
    }
}

impl IsolatedSurfaceBackend for VirtualizationFrameworkBackend {
    fn evidence_class(&self) -> ProofEvidenceClass {
        ProofEvidenceClass::VirtualizationFramework
    }

    fn boot(&mut self) -> HarnessResult<GuestFrame> {
        self.boot_attempted = true;
        Err(HarnessError::backend_unavailable(DRY_RUN_MESSAGE))
    }

    fn observe_frame(&self) -> HarnessResult<GuestFrame> {
        Err(HarnessError::backend_unavailable(DRY_RUN_MESSAGE))
    }

    fn inject_guest_local(&mut self, _action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        Err(HarnessError::backend_unavailable(DRY_RUN_MESSAGE))
    }

    fn stop_fence_first(&mut self) -> HarnessResult<()> {
        self.fenced = true;
        Err(HarnessError::backend_unavailable(DRY_RUN_MESSAGE))
    }

    fn destroy(&mut self) -> HarnessResult<()> {
        self.fenced = true;
        Err(HarnessError::backend_unavailable(DRY_RUN_MESSAGE))
    }

    fn is_booted(&self) -> bool {
        false
    }
}
