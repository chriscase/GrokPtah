//! Contained Browser substrate stub — Sep 18 pivot path, not current PASS.
//!
//! Honest `ProofEvidenceClass::ContainedBrowser` label with lifecycle hooks that
//! fail closed until real browser isolation lands. This is the documented miss
//! path when Virtualization.framework proof does not complete by Sep 18 2026.

use crate::backend::IsolatedSurfaceBackend;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::simulator::{GuestFrame, GuestLocalAction, InjectOutcome};

const STUB_MESSAGE: &str =
    "Contained Browser isolation is not implemented; this is the Sep 18 pivot stub only";

/// Fail-closed Contained Browser backend. Every operation returns
/// `BackendUnavailable` — never claim PASS from this stub.
#[derive(Debug, Clone, Default)]
pub struct ContainedBrowserBackend {
    fenced: bool,
}

impl ContainedBrowserBackend {
    pub fn new() -> Self {
        Self { fenced: false }
    }

    pub fn is_fenced(&self) -> bool {
        self.fenced
    }
}

impl IsolatedSurfaceBackend for ContainedBrowserBackend {
    fn evidence_class(&self) -> ProofEvidenceClass {
        ProofEvidenceClass::ContainedBrowser
    }

    fn boot(&mut self) -> HarnessResult<GuestFrame> {
        Err(HarnessError::backend_unavailable(STUB_MESSAGE))
    }

    fn observe_frame(&self) -> HarnessResult<GuestFrame> {
        Err(HarnessError::backend_unavailable(STUB_MESSAGE))
    }

    fn inject_guest_local(&mut self, _action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        Err(HarnessError::backend_unavailable(STUB_MESSAGE))
    }

    fn stop_fence_first(&mut self) -> HarnessResult<()> {
        self.fenced = true;
        Err(HarnessError::backend_unavailable(STUB_MESSAGE))
    }

    fn destroy(&mut self) -> HarnessResult<()> {
        self.fenced = true;
        Err(HarnessError::backend_unavailable(STUB_MESSAGE))
    }

    fn is_booted(&self) -> bool {
        false
    }
}
