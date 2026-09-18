//! Durable session lifecycle for reversible coding worktree disposition.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{SessionError, SessionResult};

pub const LIFECYCLE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Active,
    Paused,
    Settling,
    Settled,
    Stopping,
    Stopped,
    /// Recorded only after backend worktree destroy is confirmed.
    Destroyed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDisposition {
    Accepted,
    Discarded,
    KeptForReview,
    /// Apply may have partially happened; fail-closed, no auto-retry.
    Uncertain,
    /// Local Stop requested. Not isolation PASS.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLifecycle {
    pub schema_version: u32,
    pub phase: SessionPhase,
    pub disposition: Option<SessionDisposition>,
    /// True once Accept apply begins until a definitive outcome is recorded.
    pub apply_in_flight: bool,
    /// True when apply may have partially happened.
    pub apply_uncertain: bool,
    /// Settlement fence: further staging or disposition changes are rejected.
    pub settlement_fenced: bool,
    /// Local Pause fence: further staging/settlement is rejected; Stop remains legal.
    #[serde(default)]
    pub pause_fenced: bool,
    /// True only after worktree destroy is confirmed.
    #[serde(default)]
    pub destroy_confirmed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SessionLifecycle {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            phase: SessionPhase::Active,
            disposition: None,
            apply_in_flight: false,
            apply_uncertain: false,
            settlement_fenced: false,
            pause_fenced: false,
            destroy_confirmed: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn allows_staging(&self) -> bool {
        self.phase == SessionPhase::Active && !self.settlement_fenced && !self.pause_fenced
    }

    pub fn allows_settlement(&self) -> bool {
        self.phase == SessionPhase::Active && !self.settlement_fenced && !self.pause_fenced
    }

    pub fn allows_pause(&self) -> bool {
        self.phase == SessionPhase::Active && !self.settlement_fenced && !self.pause_fenced
    }

    pub fn allows_stop(&self) -> bool {
        !matches!(self.phase, SessionPhase::Destroyed)
    }

    pub fn reconcile_invariants(&mut self) -> SessionResult<()> {
        if self.disposition == Some(SessionDisposition::Uncertain) {
            self.settlement_fenced = true;
            self.pause_fenced = true;
            if self.apply_in_flight {
                return Err(SessionError::invalid_state(
                    "restored Uncertain lifecycle cannot have apply_in_flight",
                ));
            }
        }
        if matches!(
            self.disposition,
            Some(
                SessionDisposition::Accepted
                    | SessionDisposition::Discarded
                    | SessionDisposition::KeptForReview
            )
        ) {
            self.settlement_fenced = true;
            self.pause_fenced = true;
            if self.phase == SessionPhase::Active {
                return Err(SessionError::invalid_state(
                    "restored settled disposition cannot re-enter Active",
                ));
            }
        }
        if self.settlement_fenced && self.phase == SessionPhase::Active {
            return Err(SessionError::invalid_state(
                "fenced Active lifecycle is invalid",
            ));
        }
        if self.pause_fenced && self.phase == SessionPhase::Active {
            return Err(SessionError::invalid_state(
                "pause-fenced Active lifecycle is invalid",
            ));
        }
        if self.phase == SessionPhase::Paused && !self.pause_fenced {
            return Err(SessionError::invalid_state(
                "Paused lifecycle requires pause_fenced",
            ));
        }
        if self.phase == SessionPhase::Destroyed && !self.destroy_confirmed {
            return Err(SessionError::invalid_state(
                "Destroyed lifecycle requires confirmed worktree destroy",
            ));
        }
        Ok(())
    }

    pub fn begin_pause(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if !self.allows_pause() {
            return Err(SessionError::invalid_state(
                "pause requires Active unfenced lifecycle",
            ));
        }
        self.pause_fenced = true;
        self.phase = SessionPhase::Paused;
        self.updated_at = now;
        Ok(())
    }

    pub fn begin_stop(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if !self.allows_stop() {
            return Err(SessionError::invalid_state("session is already destroyed"));
        }
        if self.phase == SessionPhase::Stopping {
            return Ok(());
        }
        self.pause_fenced = true;
        self.settlement_fenced = true;
        if self.apply_in_flight || self.apply_uncertain {
            self.disposition = Some(SessionDisposition::Uncertain);
        } else if self.disposition.is_none() {
            self.disposition = Some(SessionDisposition::Stopped);
        }
        self.apply_in_flight = false;
        self.phase = SessionPhase::Stopping;
        self.updated_at = now;
        Ok(())
    }

    pub fn complete_destroy(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if self.phase != SessionPhase::Stopping {
            return Err(SessionError::invalid_state(
                "destroy requires Stopping phase",
            ));
        }
        self.destroy_confirmed = true;
        self.phase = SessionPhase::Destroyed;
        self.updated_at = now;
        Ok(())
    }

    pub fn complete_stop_unconfirmed(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if self.phase != SessionPhase::Stopping {
            return Err(SessionError::invalid_state("stop requires Stopping phase"));
        }
        self.destroy_confirmed = false;
        self.phase = SessionPhase::Stopped;
        if self.disposition.is_none() {
            self.disposition = Some(SessionDisposition::Stopped);
        }
        self.updated_at = now;
        Ok(())
    }

    pub fn begin_settlement(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if !self.allows_settlement() {
            return Err(SessionError::invalid_state(
                "settlement requires Active unfenced lifecycle",
            ));
        }
        self.phase = SessionPhase::Settling;
        self.updated_at = now;
        Ok(())
    }

    pub fn complete_settlement(
        &mut self,
        disposition: SessionDisposition,
        now: DateTime<Utc>,
    ) -> SessionResult<()> {
        if self.phase != SessionPhase::Settling {
            return Err(SessionError::invalid_state(
                "complete_settlement requires Settling phase",
            ));
        }
        self.disposition = Some(disposition);
        self.settlement_fenced = true;
        self.pause_fenced = true;
        self.apply_in_flight = false;
        self.phase = SessionPhase::Settled;
        self.updated_at = now;
        Ok(())
    }

    pub fn mark_apply_in_flight(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if self.phase != SessionPhase::Settling {
            return Err(SessionError::invalid_state("apply requires Settling phase"));
        }
        self.apply_in_flight = true;
        self.updated_at = now;
        Ok(())
    }

    pub fn mark_apply_uncertain(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if !self.apply_in_flight {
            return Err(SessionError::invalid_state(
                "uncertain apply requires apply_in_flight",
            ));
        }
        self.apply_uncertain = true;
        self.disposition = Some(SessionDisposition::Uncertain);
        self.settlement_fenced = true;
        self.pause_fenced = true;
        self.apply_in_flight = false;
        self.phase = SessionPhase::Settled;
        self.updated_at = now;
        Ok(())
    }

    /// Restart recovery: never auto-Accept; uncertain if apply was in flight.
    pub fn recover_after_restart(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if matches!(
            self.phase,
            SessionPhase::Settled | SessionPhase::Stopped | SessionPhase::Destroyed
        ) {
            return Ok(());
        }
        if self.apply_in_flight || self.apply_uncertain {
            self.disposition = Some(SessionDisposition::Uncertain);
        }
        self.settlement_fenced = true;
        self.pause_fenced = true;
        self.apply_in_flight = false;
        self.phase = SessionPhase::Settled;
        self.updated_at = now;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn hostile_restore_of_uncertain_cannot_reenable_staging() {
        let mut lifecycle = SessionLifecycle::new(now());
        lifecycle.begin_settlement(now()).expect("begin settlement");
        lifecycle
            .mark_apply_in_flight(now())
            .expect("apply in flight");
        lifecycle.mark_apply_uncertain(now()).expect("uncertain");

        lifecycle.phase = SessionPhase::Active;
        lifecycle.settlement_fenced = false;
        lifecycle.apply_in_flight = false;

        assert!(lifecycle.reconcile_invariants().is_err());
        assert!(!lifecycle.allows_staging());
        assert!(!lifecycle.allows_pause());
    }

    #[test]
    fn hostile_restore_of_accepted_cannot_reenter_active() {
        let mut lifecycle = SessionLifecycle::new(now());
        lifecycle.begin_settlement(now()).expect("begin settlement");
        lifecycle
            .complete_settlement(SessionDisposition::Accepted, now())
            .expect("accept");

        lifecycle.phase = SessionPhase::Active;
        lifecycle.settlement_fenced = false;
        lifecycle.pause_fenced = false;

        assert!(lifecycle.reconcile_invariants().is_err());
        assert!(!lifecycle.allows_settlement());
        assert!(!lifecycle.allows_pause());
        assert!(!lifecycle.allows_staging());
    }

    #[test]
    fn restart_with_apply_in_flight_becomes_uncertain() {
        let mut lifecycle = SessionLifecycle::new(now());
        lifecycle.begin_settlement(now()).unwrap();
        lifecycle.mark_apply_in_flight(now()).unwrap();

        lifecycle.recover_after_restart(now()).unwrap();
        assert_eq!(lifecycle.disposition, Some(SessionDisposition::Uncertain));
        assert!(lifecycle.settlement_fenced);
        assert!(!lifecycle.apply_in_flight);
    }
}
