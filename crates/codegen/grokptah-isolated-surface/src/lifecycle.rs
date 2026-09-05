//! Durable guest lifecycle for the Isolated Surface Proof Harness.
//!
//! Phases model the Windowed Coding Run v0 contract. `Uncertain` is a
//! fail-closed disposition after possible guest input — not a resumable phase.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, HarnessResult};

pub const LIFECYCLE_SCHEMA_VERSION: u32 = 1;

/// Live lifecycle phases for an isolated guest surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestLifecyclePhase {
    NotStarted,
    Booting,
    Ready,
    Acting,
    Stopping,
    Destroyed,
}

/// Fail-closed disposition overlay. Set when guest input may have occurred but
/// the outcome cannot be established (crash, restart mid-inject, Stop race).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestLifecycleDisposition {
    Uncertain,
    Stopped,
}

/// Evidence class for proof artifacts. Simulator output never qualifies as a
/// packaged Virtualization.framework proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofEvidenceClass {
    SyntheticHarnessIneligible,
    VirtualizationFramework,
    ContainedBrowser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestLifecycle {
    pub schema_version: u32,
    pub surface_id: String,
    pub phase: GuestLifecyclePhase,
    pub disposition: Option<GuestLifecycleDisposition>,
    /// True once Stop is requested or Uncertain is recorded. Further inject is rejected.
    pub inject_fenced: bool,
    /// True from the first guest-local inject attempt until a definitive outcome.
    pub guest_input_possible: bool,
    pub evidence_class: ProofEvidenceClass,
    pub frame_epoch: u64,
    pub actions_completed: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GuestLifecycle {
    pub fn new(surface_id: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            surface_id: surface_id.into(),
            phase: GuestLifecyclePhase::NotStarted,
            disposition: None,
            inject_fenced: false,
            guest_input_possible: false,
            evidence_class: ProofEvidenceClass::SyntheticHarnessIneligible,
            frame_epoch: 0,
            actions_completed: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_live(&self) -> bool {
        !matches!(
            self.phase,
            GuestLifecyclePhase::NotStarted
                | GuestLifecyclePhase::Stopping
                | GuestLifecyclePhase::Destroyed
        )
    }

    pub fn allows_inject(&self) -> bool {
        self.phase == GuestLifecyclePhase::Ready && !self.inject_fenced
    }

    pub fn begin_boot(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        self.require_phase(GuestLifecyclePhase::NotStarted, "begin_boot")?;
        self.advance(GuestLifecyclePhase::Booting, now);
        Ok(())
    }

    pub fn complete_boot(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        self.require_phase(GuestLifecyclePhase::Booting, "complete_boot")?;
        self.advance(GuestLifecyclePhase::Ready, now);
        Ok(())
    }

    pub fn begin_act(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        if self.inject_fenced {
            return Err(HarnessError::inject_fenced(
                "inject is fenced after Stop or Uncertain disposition",
            ));
        }
        self.require_phase(GuestLifecyclePhase::Ready, "begin_act")?;
        self.guest_input_possible = true;
        self.advance(GuestLifecyclePhase::Acting, now);
        Ok(())
    }

    pub fn complete_act(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        self.require_phase(GuestLifecyclePhase::Acting, "complete_act")?;
        self.guest_input_possible = false;
        self.actions_completed = self.actions_completed.saturating_add(1);
        self.frame_epoch = self.frame_epoch.saturating_add(1);
        self.advance(GuestLifecyclePhase::Ready, now);
        Ok(())
    }

    /// Fail-closed path after possible guest input with unknown outcome.
    pub fn mark_uncertain(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        if !self.guest_input_possible && self.phase != GuestLifecyclePhase::Acting {
            return Err(HarnessError::invalid_state(
                "uncertain disposition requires possible guest input",
            ));
        }
        self.disposition = Some(GuestLifecycleDisposition::Uncertain);
        self.inject_fenced = true;
        self.advance(self.phase, now);
        Ok(())
    }

    pub fn begin_stop(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        if matches!(self.phase, GuestLifecyclePhase::Destroyed) {
            return Err(HarnessError::invalid_state("surface is already destroyed"));
        }
        if matches!(self.phase, GuestLifecyclePhase::Stopping) {
            return Ok(());
        }
        self.inject_fenced = true;
        if self.disposition != Some(GuestLifecycleDisposition::Uncertain) {
            self.disposition = Some(GuestLifecycleDisposition::Stopped);
        }
        self.advance(GuestLifecyclePhase::Stopping, now);
        Ok(())
    }

    pub fn complete_destroy(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        let legal = matches!(
            self.phase,
            GuestLifecyclePhase::Stopping
                | GuestLifecyclePhase::Booting
                | GuestLifecyclePhase::Ready
        ) || (self.phase == GuestLifecyclePhase::Acting && self.inject_fenced);
        if !legal {
            return Err(HarnessError::invalid_state(
                "destroy requires Stopping or a fenced Acting surface",
            ));
        }
        self.advance(GuestLifecyclePhase::Destroyed, now);
        Ok(())
    }

    /// Restart recovery: if guest input was possible and outcome unknown, land in Uncertain.
    pub fn recover_after_restart(&mut self, now: DateTime<Utc>) -> HarnessResult<()> {
        if self.phase == GuestLifecyclePhase::Destroyed {
            return Ok(());
        }
        if self.guest_input_possible {
            self.disposition = Some(GuestLifecycleDisposition::Uncertain);
            self.inject_fenced = true;
            self.advance(GuestLifecyclePhase::Stopping, now);
        } else if self.is_live() {
            self.inject_fenced = true;
            self.disposition = Some(GuestLifecycleDisposition::Stopped);
            self.advance(GuestLifecyclePhase::Stopping, now);
        }
        Ok(())
    }

    fn require_phase(&self, expected: GuestLifecyclePhase, op: &str) -> HarnessResult<()> {
        if self.phase != expected {
            return Err(HarnessError::invalid_state(format!(
                "{op} requires {:?}, found {:?}",
                expected, self.phase
            )));
        }
        Ok(())
    }

    fn advance(&mut self, next: GuestLifecyclePhase, now: DateTime<Utc>) {
        if now < self.updated_at {
            // Monotonic clock is a harness invariant; treat regression as no-op advance.
        }
        self.phase = next;
        self.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn happy_path_transitions() {
        let mut lifecycle = GuestLifecycle::new("surface-1", now());
        let t0 = lifecycle.updated_at;
        lifecycle.begin_boot(t0).unwrap();
        lifecycle
            .complete_boot(t0 + chrono::Duration::milliseconds(1))
            .unwrap();
        lifecycle
            .begin_act(t0 + chrono::Duration::milliseconds(2))
            .unwrap();
        lifecycle
            .complete_act(t0 + chrono::Duration::milliseconds(3))
            .unwrap();
        assert_eq!(lifecycle.phase, GuestLifecyclePhase::Ready);
        assert_eq!(lifecycle.actions_completed, 1);
        assert!(!lifecycle.inject_fenced);
    }

    #[test]
    fn uncertain_fences_inject() {
        let mut lifecycle = GuestLifecycle::new("surface-1", now());
        let t0 = lifecycle.updated_at;
        lifecycle.begin_boot(t0).unwrap();
        lifecycle
            .complete_boot(t0 + chrono::Duration::milliseconds(1))
            .unwrap();
        lifecycle
            .begin_act(t0 + chrono::Duration::milliseconds(2))
            .unwrap();
        lifecycle
            .mark_uncertain(t0 + chrono::Duration::milliseconds(3))
            .unwrap();
        assert!(lifecycle.inject_fenced);
        assert_eq!(
            lifecycle.disposition,
            Some(GuestLifecycleDisposition::Uncertain)
        );
        lifecycle
            .begin_act(t0 + chrono::Duration::milliseconds(4))
            .unwrap_err();
    }
}
