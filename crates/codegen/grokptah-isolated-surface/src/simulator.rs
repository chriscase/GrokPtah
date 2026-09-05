//! Deterministic synthetic guest for the proof harness.
//!
//! This backend never touches host pointer/keyboard/clipboard. It models frame
//! delivery and guest-local inject only. It is ineligible for Virtualization.framework
//! qualification.

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, HarnessResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticGuestAction {
    ClickGuestButton,
    TypeGuestText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestFrame {
    pub epoch: u64,
    pub digest: String,
    pub guest_button_pressed: bool,
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
}

impl SyntheticGuest {
    pub fn new() -> Self {
        Self {
            booted: false,
            frame_epoch: 0,
            guest_button_pressed: false,
            crash_on_next_inject: false,
            uncertain_on_next_inject: false,
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
        GuestFrame {
            epoch: self.frame_epoch,
            digest: frame_digest(self.frame_epoch, self.guest_button_pressed),
            guest_button_pressed: self.guest_button_pressed,
        }
    }

    pub fn inject(&mut self, action: SyntheticGuestAction) -> HarnessResult<InjectOutcome> {
        if !self.booted {
            return Err(HarnessError::invalid_state("guest is not booted"));
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
            SyntheticGuestAction::ClickGuestButton => {
                self.guest_button_pressed = true;
            }
            SyntheticGuestAction::TypeGuestText => {
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
