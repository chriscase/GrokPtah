//! Contained Browser substrate v0 — Sep 18 pivot path, admission-false.
//!
//! Browser-only scope: guest-local DOM inject and frame observation only.
//! Not arbitrary desktop-app control, not Virtualization.framework, and never
//! host CGEvent / AX / Apple Events / clipboard / shared dirs / guest NIC paths.
//!
//! Default builds use an in-process browser simulator (Linux CI friendly).
//! Optional `browser-engine` feature remains default-off; when enabled the
//! substrate still fails closed until a real isolated browser engine is wired.

use crate::backend::IsolatedSurfaceBackend;
use crate::captured_frame::BoundedCapturedFrame;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::simulator::{GuestFrame, GuestLocalAction, InjectOutcome};

#[cfg(feature = "browser-engine")]
const ENGINE_UNAVAILABLE: &str =
    "Contained Browser engine feature is enabled but isolation is not proven; boot fails closed";

/// Fail-closed when the optional browser engine path is selected but unwired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubstrateMode {
    /// In-process browser frame simulator — exercises SPI contract, not isolation PASS.
    #[cfg_attr(feature = "browser-engine", allow(dead_code))]
    Simulator,
    #[cfg(feature = "browser-engine")]
    /// Reserved for native browser engine wiring; always fails closed in v0.
    EngineUnavailable,
}

/// Contained Browser backend v0. Honest `ProofEvidenceClass::ContainedBrowser`
/// label with a full browsing-state lifecycle. Isolation is **not** proven in
/// v0 — admission stays false and artifacts must carry the dry-run nonclaim.
#[derive(Debug)]
pub struct ContainedBrowserBackend {
    mode: SubstrateMode,
    booted: bool,
    frame_epoch: u64,
    guest_link_clicked: bool,
    inject_fenced: bool,
    uncertain_on_next_inject: bool,
    crash_on_next_inject: bool,
}

impl ContainedBrowserBackend {
    pub fn new() -> Self {
        Self::with_mode(default_substrate_mode())
    }

    fn with_mode(mode: SubstrateMode) -> Self {
        Self {
            mode,
            booted: false,
            frame_epoch: 0,
            guest_link_clicked: false,
            inject_fenced: false,
            uncertain_on_next_inject: false,
            crash_on_next_inject: false,
        }
    }

    /// v0 never proves browser isolation — substrate rehearsal only.
    pub fn isolation_proof_available(&self) -> bool {
        false
    }

    pub fn substrate_mode_label(&self) -> &'static str {
        match self.mode {
            SubstrateMode::Simulator => "simulator",
            #[cfg(feature = "browser-engine")]
            SubstrateMode::EngineUnavailable => "engine_unavailable",
        }
    }

    pub fn schedule_uncertain_on_next_inject(&mut self) {
        self.uncertain_on_next_inject = true;
    }

    pub fn schedule_crash_on_next_inject(&mut self) {
        self.crash_on_next_inject = true;
    }

    fn current_capture(&self) -> HarnessResult<BoundedCapturedFrame> {
        BoundedCapturedFrame::admit_simulator_capture(self.frame_epoch, self.guest_link_clicked)
    }

    fn current_frame(&self) -> HarnessResult<GuestFrame> {
        Ok(self
            .current_capture()?
            .to_guest_frame(self.guest_link_clicked))
    }

    fn fence_inject(&mut self) {
        self.inject_fenced = true;
    }

    fn boot_simulator(&mut self) -> HarnessResult<GuestFrame> {
        if self.booted {
            return Err(HarnessError::invalid_state("browser guest already booted"));
        }
        self.booted = true;
        self.frame_epoch = 1;
        self.current_frame()
    }

    fn inject_browser_local(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        if !self.booted {
            return Err(HarnessError::invalid_state("browser guest is not booted"));
        }
        if self.inject_fenced {
            return Err(HarnessError::inject_fenced(
                "browser guest inject is fenced",
            ));
        }

        if self.crash_on_next_inject {
            self.crash_on_next_inject = false;
            return Ok(InjectOutcome::Crash);
        }
        if self.uncertain_on_next_inject {
            self.uncertain_on_next_inject = false;
            return Ok(InjectOutcome::Uncertain);
        }

        let before = self.current_frame()?;
        match action {
            GuestLocalAction::ClickGuestButton => {
                // Guest-local link click inside contained browser DOM only.
                self.guest_link_clicked = true;
            }
            GuestLocalAction::TypeGuestText => {
                // Guest-local text field inside browser surface only.
            }
        }
        self.frame_epoch = self.frame_epoch.saturating_add(1);
        let after = self.current_frame()?;
        let guest_local_change = before.digest != after.digest;
        Ok(InjectOutcome::Changed(crate::simulator::FrameDelta {
            before_epoch: before.epoch,
            after_epoch: after.epoch,
            before_digest: before.digest,
            after_digest: after.digest,
            guest_local_change,
        }))
    }
}

impl Default for ContainedBrowserBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn default_substrate_mode() -> SubstrateMode {
    #[cfg(feature = "browser-engine")]
    {
        SubstrateMode::EngineUnavailable
    }
    #[cfg(not(feature = "browser-engine"))]
    {
        SubstrateMode::Simulator
    }
}

impl IsolatedSurfaceBackend for ContainedBrowserBackend {
    fn evidence_class(&self) -> ProofEvidenceClass {
        ProofEvidenceClass::ContainedBrowser
    }

    fn boot(&mut self) -> HarnessResult<GuestFrame> {
        match self.mode {
            SubstrateMode::Simulator => self.boot_simulator(),
            #[cfg(feature = "browser-engine")]
            SubstrateMode::EngineUnavailable => {
                Err(HarnessError::backend_unavailable(ENGINE_UNAVAILABLE))
            }
        }
    }

    fn observe_frame(&self) -> HarnessResult<GuestFrame> {
        if !self.booted {
            return Err(HarnessError::invalid_state("browser guest is not booted"));
        }
        self.current_frame()
    }

    fn inject_guest_local(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        match self.mode {
            SubstrateMode::Simulator => self.inject_browser_local(action),
            #[cfg(feature = "browser-engine")]
            SubstrateMode::EngineUnavailable => {
                Err(HarnessError::backend_unavailable(ENGINE_UNAVAILABLE))
            }
        }
    }

    /// Production fence: halts browser guest-local inject dispatch before teardown.
    fn stop_fence_first(&mut self) -> HarnessResult<()> {
        self.fence_inject();
        Ok(())
    }

    fn destroy(&mut self) -> HarnessResult<()> {
        self.booted = false;
        Ok(())
    }

    fn is_booted(&self) -> bool {
        self.booted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "browser-engine"))]
    #[test]
    fn contained_browser_simulator_lifecycle() {
        let mut backend = ContainedBrowserBackend::new();
        assert_eq!(
            backend.evidence_class(),
            ProofEvidenceClass::ContainedBrowser
        );
        assert!(!backend.isolation_proof_available());

        let frame = backend.boot().expect("boot");
        assert_eq!(frame.epoch, 1);
        assert!(crate::captured_frame::is_canonical_sha256_digest(
            &frame.digest
        ));
        assert_eq!(
            frame.captured_frame.as_ref().map(|ev| ev.source),
            Some(crate::captured_frame::CapturedFrameSource::SyntheticSimulator)
        );
        assert!(backend.is_booted());

        let observed = backend.observe_frame().expect("observe");
        assert_eq!(observed.epoch, 1);
        assert_eq!(observed.digest, frame.digest);

        let outcome = backend
            .inject_guest_local(GuestLocalAction::ClickGuestButton)
            .expect("inject");
        assert!(matches!(outcome, InjectOutcome::Changed(_)));
        let after = backend.observe_frame().expect("after");
        assert_ne!(after.digest, frame.digest);
        crate::captured_frame::assert_postcondition_change(&frame, &after).expect("postcondition");

        backend.stop_fence_first().expect("fence");
        backend.destroy().expect("destroy");
        assert!(!backend.is_booted());
    }

    #[test]
    fn contained_browser_never_vf_eligible() {
        let backend = ContainedBrowserBackend::new();
        assert!(!backend.evidence_class().is_vf_qualification_eligible());
    }

    #[cfg(feature = "browser-engine")]
    #[test]
    fn browser_engine_feature_fails_closed_on_boot() {
        let mut backend = ContainedBrowserBackend::new();
        assert_eq!(backend.substrate_mode_label(), "engine_unavailable");
        let err = backend.boot().expect_err("engine path fails closed");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(!backend.is_booted());
    }
}
