//! Durable session lifecycle for reversible coding worktree disposition.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{SessionError, SessionResult};

pub const LIFECYCLE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Active,
    Settling,
    Settled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDisposition {
    Accepted,
    Discarded,
    KeptForReview,
    /// Apply may have partially happened; fail-closed, no auto-retry.
    Uncertain,
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
            created_at: now,
            updated_at: now,
        }
    }

    pub fn allows_staging(&self) -> bool {
        self.phase == SessionPhase::Active && !self.settlement_fenced
    }

    pub fn allows_settlement(&self) -> bool {
        self.phase == SessionPhase::Active && !self.settlement_fenced
    }

    pub fn reconcile_invariants(&mut self) -> SessionResult<()> {
        if self.disposition == Some(SessionDisposition::Uncertain) {
            self.settlement_fenced = true;
            if self.apply_in_flight {
                return Err(SessionError::invalid_state(
                    "restored Uncertain lifecycle cannot have apply_in_flight",
                ));
            }
        }
        if self.settlement_fenced && self.phase == SessionPhase::Active {
            return Err(SessionError::invalid_state(
                "fenced Active lifecycle is invalid",
            ));
        }
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
        self.apply_in_flight = false;
        self.phase = SessionPhase::Settled;
        self.updated_at = now;
        Ok(())
    }

    /// Restart recovery: never auto-Accept; uncertain if apply was in flight.
    pub fn recover_after_restart(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        if self.phase == SessionPhase::Settled {
            return Ok(());
        }
        if self.apply_in_flight || self.apply_uncertain {
            self.disposition = Some(SessionDisposition::Uncertain);
        }
        self.settlement_fenced = true;
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
