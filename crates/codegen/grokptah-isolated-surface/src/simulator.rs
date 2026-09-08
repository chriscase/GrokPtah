//! Deterministic synthetic guest for the proof harness.
//!
//! This backend never touches host pointer/keyboard/clipboard. It models frame
//! delivery and guest-local inject only. It is ineligible for Virtualization.framework
//! qualification.

use serde::{Deserialize, Serialize};

use crate::backend::IsolatedSurfaceBackend;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;

/// Guest-local action dispatched through the backend SPI (never host paths).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestLocalAction {
    ClickGuestButton,
    TypeGuestText,
}

/// Backward-compatible alias for pre-SPI naming.
pub type SyntheticGuestAction = GuestLocalAction;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestFrame {
    pub epoch: u64,
    pub digest: String,
    pub guest_button_pressed: bool,
    /// Contained Browser captured-frame metadata. Absent on synthetic-harness
    /// backends that still use label digests. Never contains raw frame bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_frame: Option<crate::captured_frame::CapturedFrameEvidence>,
}

impl GuestFrame {
    pub fn new(epoch: u64, digest: impl Into<String>, guest_button_pressed: bool) -> Self {
        Self {
            epoch,
            digest: digest.into(),
            guest_button_pressed,
            captured_frame: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameDelta {
    pub before_epoch: u64,
    pub after_epoch: u64,
    pub before_digest: String,
    pub after_digest: String,
    pub guest_local_change: bool,
}

#[derive(Debug)]
pub struct SyntheticGuest {
    booted: bool,
    frame_epoch: u64,
    guest_button_pressed: bool,
    crash_on_next_inject: bool,
    uncertain_on_next_inject: bool,
    inject_fenced: bool,
}

impl SyntheticGuest {
    pub fn new() -> Self {
        Self {
            booted: false,
            frame_epoch: 0,
            guest_button_pressed: false,
            crash_on_next_inject: false,
            uncertain_on_next_inject: false,
            inject_fenced: false,
        }
    }

    pub fn is_booted(&self) -> bool {
        self.booted
    }

    pub fn schedule_crash_on_inject(&mut self) {
        self.crash_on_next_inject = true;
    }

    pub fn schedule_uncertain_on_inject(&mut self) {
        self.uncertain_on_next_inject = true;
    }

    pub fn boot(&mut self) -> HarnessResult<GuestFrame> {
        if self.booted {
            return Err(HarnessError::invalid_state("guest already booted"));
        }
        self.booted = true;
        self.frame_epoch = 1;
        Ok(self.current_frame())
    }

    pub fn current_frame(&self) -> GuestFrame {
        GuestFrame::new(
            self.frame_epoch,
            frame_digest(self.frame_epoch, self.guest_button_pressed),
            self.guest_button_pressed,
        )
    }

    pub fn inject(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        if !self.booted {
            return Err(HarnessError::invalid_state("guest is not booted"));
        }
        if self.inject_fenced {
            return Err(HarnessError::inject_fenced("guest inject is fenced"));
        }

        if self.crash_on_next_inject {
            self.crash_on_next_inject = false;
            return Ok(InjectOutcome::Crash);
        }
        if self.uncertain_on_next_inject {
            self.uncertain_on_next_inject = false;
            return Ok(InjectOutcome::Uncertain);
        }

        let before = self.current_frame();
        match action {
            GuestLocalAction::ClickGuestButton => {
                self.guest_button_pressed = true;
            }
            GuestLocalAction::TypeGuestText => {
                // Guest-local only; no host keyboard path exists in the harness.
            }
        }
        self.frame_epoch = self.frame_epoch.saturating_add(1);
        let after = self.current_frame();
        let guest_local_change = before.digest != after.digest;
        Ok(InjectOutcome::Changed(FrameDelta {
            before_epoch: before.epoch,
            after_epoch: after.epoch,
            before_digest: before.digest,
            after_digest: after.digest,
            guest_local_change,
        }))
    }

    pub fn shutdown(&mut self) -> HarnessResult<()> {
        self.booted = false;
        Ok(())
    }

    pub fn fence_inject(&mut self) {
        self.inject_fenced = true;
    }
}

impl IsolatedSurfaceBackend for SyntheticGuest {
    fn evidence_class(&self) -> ProofEvidenceClass {
        ProofEvidenceClass::Synthetic
    }

    fn boot(&mut self) -> HarnessResult<GuestFrame> {
        SyntheticGuest::boot(self)
    }

    fn observe_frame(&self) -> HarnessResult<GuestFrame> {
        if !self.booted {
            return Err(HarnessError::invalid_state("guest is not booted"));
        }
        Ok(self.current_frame())
    }

    fn inject_guest_local(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        self.inject(action)
    }

    /// Test-only explicit fence: halts synthetic guest inject dispatch.
    /// Production adapters must not rely on a trait default — implement fence with
    /// real ack or explicit failure.
    fn stop_fence_first(&mut self) -> HarnessResult<()> {
        self.fence_inject();
        Ok(())
    }

    fn destroy(&mut self) -> HarnessResult<()> {
        self.shutdown()
    }

    fn is_booted(&self) -> bool {
        self.booted
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectOutcome {
    Changed(FrameDelta),
    Uncertain,
    Crash,
}

fn frame_digest(epoch: u64, guest_button_pressed: bool) -> String {
    format!(
        "sha256:synthetic-frame:{epoch}:btn={guest_button_pressed}",
        epoch = epoch,
        guest_button_pressed = guest_button_pressed
    )
}

impl Default for SyntheticGuest {
    fn default() -> Self {
        Self::new()
    }
}

/// Test/diagnostic wrapper that can fail selected backend SPI hooks.
#[derive(Debug)]
pub struct FaultInjectingBackend<B: IsolatedSurfaceBackend> {
    inner: B,
    pub fail_stop_fence: bool,
    pub fail_inject_with_err: bool,
    pub fail_destroy: bool,
}

impl<B: IsolatedSurfaceBackend> FaultInjectingBackend<B> {
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            fail_stop_fence: false,
            fail_inject_with_err: false,
            fail_destroy: false,
        }
    }
}

impl<B: IsolatedSurfaceBackend> IsolatedSurfaceBackend for FaultInjectingBackend<B> {
    fn evidence_class(&self) -> ProofEvidenceClass {
        self.inner.evidence_class()
    }

    fn boot(&mut self) -> HarnessResult<GuestFrame> {
        self.inner.boot()
    }

    fn observe_frame(&self) -> HarnessResult<GuestFrame> {
        self.inner.observe_frame()
    }

    fn inject_guest_local(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        if self.fail_inject_with_err {
            return Err(HarnessError::backend_unavailable(
                "injected backend inject error for fault matrix",
            ));
        }
        self.inner.inject_guest_local(action)
    }

    fn stop_fence_first(&mut self) -> HarnessResult<()> {
        if self.fail_stop_fence {
            self.inner.stop_fence_first()?;
            return Err(HarnessError::backend_unavailable(
                "injected backend stop_fence_first error for fault matrix",
            ));
        }
        self.inner.stop_fence_first()
    }

    fn destroy(&mut self) -> HarnessResult<()> {
        if self.fail_destroy {
            return Err(HarnessError::backend_unavailable(
                "injected backend destroy error for fault matrix",
            ));
        }
        self.inner.destroy()
    }

    fn is_booted(&self) -> bool {
        self.inner.is_booted()
    }
}
