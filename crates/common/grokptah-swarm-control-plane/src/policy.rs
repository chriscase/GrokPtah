//! Admission, budget, review, and failure policy.
//!
//! Every policy carries an explicit ceiling. A specification that omits a
//! bound does not become unbounded — [`Default`] supplies a conservative value
//! and validation rejects anything above the hard ceiling, mirroring the
//! bounded-plan rules the durable manager already enforces.

use serde::{Deserialize, Serialize};

use crate::error::{SwarmError, SwarmResult};
use crate::ids::TaskId;

/// Hard ceiling on nodes in one task graph. Matches the durable manager's
/// per-plan step ceiling so a swarm cannot outgrow the coordinator that will
/// eventually project it.
pub const MAX_TASKS: usize = 64;
/// Hard ceiling on worker specifications in one swarm.
pub const MAX_WORKERS: usize = 32;
/// Hard ceiling on simultaneously running tasks.
pub const MAX_IN_FLIGHT: u32 = 16;
/// Hard ceiling on direct dependents of any single task.
pub const MAX_FAN_OUT: u32 = 16;
/// Hard ceiling on declared dependencies of any single task.
pub const MAX_DEPENDENCIES: usize = 16;
/// Hard ceiling on reviewers gating one synthesis task.
pub const MAX_REVIEWERS: usize = 16;
/// Hard ceiling on dispatch attempts across the whole swarm.
pub const MAX_TOTAL_DISPATCHES: u32 = 256;
/// Hard ceiling on wall-clock budget, in seconds.
pub const MAX_WALL_CLOCK_SECS: u64 = 12 * 60 * 60;

/// Concurrency and fan-out bounds applied at admission time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionPolicy {
    /// Maximum tasks that may hold a live dispatch simultaneously.
    pub max_in_flight: u32,
    /// Maximum direct dependents any one task may have. Checked during graph
    /// validation, not at dispatch time, so an over-wide graph is rejected
    /// before any child is spawned.
    pub max_fan_out: u32,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            max_in_flight: 4,
            max_fan_out: 8,
        }
    }
}

impl AdmissionPolicy {
    pub fn validate(&self) -> SwarmResult<()> {
        if self.max_in_flight == 0 || self.max_in_flight > MAX_IN_FLIGHT {
            return Err(SwarmError::bound(format!(
                "maxInFlight must be between 1 and {MAX_IN_FLIGHT}"
            )));
        }
        if self.max_fan_out == 0 || self.max_fan_out > MAX_FAN_OUT {
            return Err(SwarmError::bound(format!(
                "maxFanOut must be between 1 and {MAX_FAN_OUT}"
            )));
        }
        Ok(())
    }
}

/// Total campaign spend bounds. Exhausting a budget stops further dispatch; it
/// never cancels a child that is already running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetPolicy {
    /// Maximum dispatch attempts the whole swarm may make.
    pub max_total_dispatches: u32,
    /// Maximum wall-clock seconds from swarm creation.
    pub max_wall_clock_secs: u64,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            max_total_dispatches: 64,
            max_wall_clock_secs: 60 * 60,
        }
    }
}

impl BudgetPolicy {
    pub fn validate(&self) -> SwarmResult<()> {
        if self.max_total_dispatches == 0 || self.max_total_dispatches > MAX_TOTAL_DISPATCHES {
            return Err(SwarmError::bound(format!(
                "maxTotalDispatches must be between 1 and {MAX_TOTAL_DISPATCHES}"
            )));
        }
        if self.max_wall_clock_secs == 0 || self.max_wall_clock_secs > MAX_WALL_CLOCK_SECS {
            return Err(SwarmError::bound(format!(
                "maxWallClockSecs must be between 1 and {MAX_WALL_CLOCK_SECS}"
            )));
        }
        Ok(())
    }
}

/// How many approving reviewers a synthesis task requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "rule", deny_unknown_fields)]
pub enum QuorumRule {
    /// Every reviewer must approve.
    Unanimous,
    /// Strictly more than half of the reviewers must approve.
    Majority,
    /// An explicit approval count must be met.
    AtLeast { approvals: u32 },
}

impl QuorumRule {
    /// Approvals required for `reviewer_count` reviewers.
    pub fn required_approvals(self, reviewer_count: u32) -> u32 {
        match self {
            Self::Unanimous => reviewer_count,
            Self::Majority => reviewer_count / 2 + 1,
            Self::AtLeast { approvals } => approvals,
        }
    }

    fn validate(self, reviewer_count: u32) -> SwarmResult<()> {
        if let Self::AtLeast { approvals } = self {
            if approvals == 0 {
                return Err(SwarmError::invalid("quorum approvals must be positive"));
            }
            if approvals > reviewer_count {
                return Err(SwarmError::invalid(
                    "quorum approvals exceed the number of reviewers",
                ));
            }
        }
        Ok(())
    }
}

/// A synthesis gate: named reviewer tasks plus the quorum they must reach.
///
/// The gate is evaluated on reviewer *verdicts*, not on reviewer success. A
/// reviewer that runs to completion and rejects has succeeded at its job and
/// still withholds its approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewGate {
    pub reviewers: Vec<TaskId>,
    pub quorum: QuorumRule,
}

impl ReviewGate {
    pub fn validate(&self) -> SwarmResult<()> {
        if self.reviewers.is_empty() {
            return Err(SwarmError::invalid("review gate must name a reviewer"));
        }
        if self.reviewers.len() > MAX_REVIEWERS {
            return Err(SwarmError::bound(format!(
                "review gate may name at most {MAX_REVIEWERS} reviewers"
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for reviewer in &self.reviewers {
            reviewer.validate()?;
            if !seen.insert(reviewer.clone()) {
                return Err(SwarmError::invalid("review gate reviewers must be unique"));
            }
        }
        let count = u32::try_from(self.reviewers.len())
            .map_err(|_| SwarmError::bound("reviewer count does not fit in u32"))?;
        self.quorum.validate(count)
    }
}

/// What happens to the rest of the graph when one task fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// Block the failed task's transitive dependents and let independent
    /// branches continue. Replacement work is never invented implicitly.
    #[default]
    BlockDependents,
    /// Treat any failure as fatal and cancel the whole swarm.
    CancelSwarm,
}
