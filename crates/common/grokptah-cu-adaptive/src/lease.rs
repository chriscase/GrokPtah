//! Run leases, compare-and-swap, frame tokens, and stale-frame refusal.
//!
//! Two fences guard every committed step, and they guard different things.
//!
//! The **lease** answers "am I still the one driving?". It is held by exactly
//! one holder, carries a monotonic `version` that every mutation must
//! compare-and-swap against, and carries a monotonic `epoch` that a pause,
//! operator takeover, cancellation, or recovery bumps. A stale approval or a
//! reconnecting client cannot revive a run whose epoch has moved on, which is
//! the failure mode a plain "paused" flag has: pausing is reversible by
//! resuming, and a takeover must not be.
//!
//! The **frame token** answers "am I still looking at what I decided from?".
//! It pins a frame's identity, its sequence, the lease epoch it was captured
//! under, and its capture time. A step may only be dispatched against the
//! exact frame it was decided on, and only while that frame is younger than
//! the profile's bound.
//!
//! Both are checked on every commit rather than once at admission, because the
//! interesting failures happen *between* deciding and acting: the operator
//! takes over, the window is rebound, thirty seconds pass while a human reads
//! an approval prompt. A run that checked at admission and trusted afterwards
//! would act on all three.

use serde::{Deserialize, Serialize};

use crate::digest::is_digest;
use crate::vocabulary::DenyReason;

/// Who holds the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseHolder {
    /// The adaptive planner/executor pair.
    Agent,
    /// A human took over. The agent may observe, never act.
    Operator,
    /// Nobody: the run was stopped or cancelled.
    None,
}

/// Why the epoch moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpochBump {
    Paused,
    Resumed,
    OperatorTakeover,
    Cancelled,
    Recovered,
}

/// A single-holder lease over one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLease {
    pub run_id: String,
    pub holder: LeaseHolder,
    /// Bumped by every control transition. Frames captured under an older
    /// epoch are refused.
    pub epoch: u64,
    /// Bumped by every accepted mutation. This is the compare-and-swap fence.
    pub version: u64,
    /// Synthetic clock reading after which the lease is no longer valid.
    pub expires_at_millis: u64,
}

impl RunLease {
    #[must_use]
    pub fn new(run_id: impl Into<String>, expires_at_millis: u64) -> Self {
        Self {
            run_id: run_id.into(),
            holder: LeaseHolder::Agent,
            epoch: 0,
            version: 1,
            expires_at_millis,
        }
    }

    /// Check that the agent may still act, at the given clock reading.
    pub fn check_agent_may_act(&self, now_millis: u64) -> Result<(), DenyReason> {
        match self.holder {
            LeaseHolder::Agent => {}
            LeaseHolder::Operator => return Err(DenyReason::LeaseLost),
            LeaseHolder::None => return Err(DenyReason::Cancelled),
        }
        if now_millis >= self.expires_at_millis {
            return Err(DenyReason::LeaseLost);
        }
        Ok(())
    }

    /// Compare-and-swap the lease version.
    ///
    /// The caller passes the version it believes it holds. A mismatch means
    /// another writer moved the run underneath it, and the mutation is refused
    /// rather than applied on top -- the difference between a lost update and
    /// a detected conflict.
    pub fn compare_and_swap(
        &mut self,
        expected_version: u64,
        now_millis: u64,
    ) -> Result<u64, DenyReason> {
        self.check_agent_may_act(now_millis)?;
        if self.version != expected_version {
            return Err(DenyReason::LeaseVersionConflict);
        }
        self.version = self.version.saturating_add(1);
        Ok(self.version)
    }

    /// Move the control epoch. Always advances, never rewinds.
    pub fn bump_epoch(&mut self, bump: EpochBump) -> u64 {
        self.epoch = self.epoch.saturating_add(1);
        self.holder = match bump {
            EpochBump::Paused | EpochBump::Resumed | EpochBump::Recovered => LeaseHolder::Agent,
            EpochBump::OperatorTakeover => LeaseHolder::Operator,
            EpochBump::Cancelled => LeaseHolder::None,
        };
        self.epoch
    }
}

/// The identity of one observed frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameToken {
    pub frame_id: String,
    /// Monotonic within a run.
    pub sequence: u64,
    /// The lease epoch this frame was captured under.
    pub epoch: u64,
    pub captured_at_millis: u64,
    /// Digest of the frame's semantic content.
    pub digest: String,
}

impl FrameToken {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.frame_id.is_empty()
            && self.frame_id.len() <= 128
            && self
                .frame_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && is_digest(&self.digest)
    }

    /// Admit a step that was decided against `self` for dispatch against
    /// `live`.
    ///
    /// The order is: schema, then epoch, then identity, then age. Epoch is
    /// checked before identity because a takeover is a more important thing to
    /// report than a frame that also happens to have moved on.
    pub fn admit(
        &self,
        live: &FrameToken,
        lease: &RunLease,
        now_millis: u64,
        max_age_millis: u64,
    ) -> Result<(), DenyReason> {
        if !self.is_well_formed() || !live.is_well_formed() {
            return Err(DenyReason::SchemaViolation);
        }
        if self.epoch != lease.epoch || live.epoch != lease.epoch {
            return Err(DenyReason::FrameEpochChanged);
        }
        if self.frame_id != live.frame_id
            || self.sequence != live.sequence
            || self.digest != live.digest
        {
            return Err(DenyReason::StaleFrame);
        }
        // A frame from the future is not fresh, it is wrong. Refusing rather
        // than clamping keeps a skewed clock from buying unlimited freshness.
        if now_millis < self.captured_at_millis {
            return Err(DenyReason::StaleFrame);
        }
        if now_millis - self.captured_at_millis > max_age_millis {
            return Err(DenyReason::StaleFrame);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::{digest_str, domain};

    fn frame(sequence: u64, epoch: u64, captured_at_millis: u64) -> FrameToken {
        FrameToken {
            frame_id: "frame-1".into(),
            sequence,
            epoch,
            captured_at_millis,
            digest: digest_str(domain::FRAME, &format!("frame-{sequence}")),
        }
    }

    #[test]
    fn compare_and_swap_detects_a_lost_update() {
        let mut lease = RunLease::new("run-1", 10_000);
        let held = lease.version;
        assert_eq!(lease.compare_and_swap(held, 0).unwrap(), held + 1);
        // A second writer still holding the old version is refused.
        assert_eq!(
            lease.compare_and_swap(held, 0).unwrap_err(),
            DenyReason::LeaseVersionConflict
        );
    }

    #[test]
    fn an_operator_takeover_is_not_undone_by_a_stale_resume() {
        let mut lease = RunLease::new("run-1", 10_000);
        lease.bump_epoch(EpochBump::OperatorTakeover);
        assert_eq!(lease.holder, LeaseHolder::Operator);
        assert_eq!(
            lease.check_agent_may_act(0).unwrap_err(),
            DenyReason::LeaseLost
        );
        // A frame captured before the takeover cannot be admitted afterwards,
        // whatever the agent believes about its freshness.
        let before = frame(1, 0, 0);
        let err = before.admit(&before, &lease, 0, 10_000).unwrap_err();
        assert_eq!(err, DenyReason::FrameEpochChanged);
    }

    #[test]
    fn cancellation_leaves_nobody_holding_the_lease() {
        let mut lease = RunLease::new("run-1", 10_000);
        lease.bump_epoch(EpochBump::Cancelled);
        assert_eq!(lease.holder, LeaseHolder::None);
        assert_eq!(
            lease.check_agent_may_act(0).unwrap_err(),
            DenyReason::Cancelled
        );
        assert_eq!(
            lease.compare_and_swap(lease.version, 0).unwrap_err(),
            DenyReason::Cancelled
        );
    }

    #[test]
    fn epochs_only_advance() {
        let mut lease = RunLease::new("run-1", 10_000);
        let mut last = lease.epoch;
        for bump in [
            EpochBump::Paused,
            EpochBump::Resumed,
            EpochBump::Recovered,
            EpochBump::OperatorTakeover,
            EpochBump::Cancelled,
        ] {
            let next = lease.bump_epoch(bump);
            assert!(next > last, "{bump:?} did not advance the epoch");
            last = next;
        }
    }

    #[test]
    fn a_superseded_frame_is_refused() {
        let lease = RunLease::new("run-1", 10_000);
        let decided_on = frame(4, 0, 0);
        let live = frame(5, 0, 0);
        assert_eq!(
            decided_on.admit(&live, &lease, 0, 10_000).unwrap_err(),
            DenyReason::StaleFrame
        );
    }

    #[test]
    fn frame_age_is_enforced_in_both_directions() {
        let lease = RunLease::new("run-1", 100_000);
        let token = frame(1, 0, 1_000);
        assert!(token.admit(&token, &lease, 1_500, 1_000).is_ok());
        assert_eq!(
            token.admit(&token, &lease, 2_500, 1_000).unwrap_err(),
            DenyReason::StaleFrame
        );
        // Captured "after" now: a skewed clock buys nothing.
        assert_eq!(
            token.admit(&token, &lease, 500, 1_000).unwrap_err(),
            DenyReason::StaleFrame
        );
    }

    #[test]
    fn an_expired_lease_stops_the_agent_even_while_it_still_holds_it() {
        let lease = RunLease::new("run-1", 1_000);
        assert!(lease.check_agent_may_act(999).is_ok());
        assert_eq!(
            lease.check_agent_may_act(1_000).unwrap_err(),
            DenyReason::LeaseLost
        );
    }

    #[test]
    fn malformed_frame_tokens_are_schema_violations() {
        let lease = RunLease::new("run-1", 10_000);
        let mut bad = frame(1, 0, 0);
        bad.digest = "short".into();
        assert_eq!(
            bad.admit(&bad, &lease, 0, 10_000).unwrap_err(),
            DenyReason::SchemaViolation
        );
        let mut bad_id = frame(1, 0, 0);
        bad_id.frame_id = "../escape".into();
        assert_eq!(
            bad_id.admit(&bad_id, &lease, 0, 10_000).unwrap_err(),
            DenyReason::SchemaViolation
        );
    }
}
