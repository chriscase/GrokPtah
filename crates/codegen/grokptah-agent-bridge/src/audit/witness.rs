//! Rollback witness seam (#443).
//!
//! Sections 02-06 of the audit design are all *internal* consistency: a
//! coherent earlier snapshot of every local file satisfies every invariant.
//! Detecting that requires state the restore cannot roll back, which means a
//! platform monotonic counter or a remote witness.
//!
//! This module defines only the seam. **No witness service, transport, or
//! network client is implemented here**, and the default boundary is
//! [`UnwitnessedBoundary`], which reports honestly that nothing is witnessed
//! rather than implying a guarantee that does not exist.

use serde::{Deserialize, Serialize};

/// The exact values a witness needs. Deliberately narrow: no journal contents,
/// no scope, no actor, no key material — a witness learns only that an
/// installation advanced to an epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WitnessBeacon {
    pub installation_id: String,
    pub manifest_epoch: u64,
    pub retention_epoch: u64,
    pub global_last_seq_floor: u64,
    pub active_generation_id: String,
    pub manifest_mac: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessVerdict {
    /// The witness confirms this installation has not moved backwards.
    Verified,
    /// The witness could not be consulted. Operation continues; no claim is made.
    Unverified(&'static str),
    /// The local epoch is behind the witness: a proven rollback.
    Rollback { local: u64, witness: u64 },
}

/// What an export receipt is allowed to say about witnessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WitnessState {
    /// No witness is configured at all. The default.
    Unwitnessed,
    /// A witness is configured and confirmed this state.
    Verified,
    /// A witness is configured but could not be reached.
    Unverified,
}

impl WitnessState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unwitnessed => "unwitnessed",
            Self::Verified => "verified",
            Self::Unverified => "unverified",
        }
    }
}

/// Implementations must be fail-closed on contradiction and fail-soft on
/// unavailability: a witness that cannot be reached must never take the host
/// down, and must never silently upgrade into an implied guarantee.
pub trait AuditWitness: Send + Sync {
    /// Called after every committed manifest write.
    fn record(&self, beacon: &WitnessBeacon);
    /// Called at open, before the ledger may become ready.
    fn check(&self, beacon: &WitnessBeacon) -> WitnessVerdict;
    /// What an export receipt may claim about this boundary.
    fn state(&self) -> WitnessState;
}

/// The default boundary: no witness exists, and every receipt says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnwitnessedBoundary;

impl AuditWitness for UnwitnessedBoundary {
    fn record(&self, _beacon: &WitnessBeacon) {}

    fn check(&self, _beacon: &WitnessBeacon) -> WitnessVerdict {
        WitnessVerdict::Unverified("no witness configured")
    }

    fn state(&self) -> WitnessState {
        WitnessState::Unwitnessed
    }
}
