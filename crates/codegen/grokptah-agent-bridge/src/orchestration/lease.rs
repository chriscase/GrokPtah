//! Durable attempt lease. A process-local boolean is not a lease.

use serde::{Deserialize, Serialize};

use super::authority::{Revision, SpineError};

/// Durable attempt lease with owner, epoch, expiry, and revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptLease {
    /// Lease identity. Distinct from attempt and run.
    pub lease_id: String,
    /// Owner process/coordinator identity.
    pub owner: String,
    /// Epoch. Stale holders cannot revive an expired epoch.
    pub epoch: u64,
    /// Expiry as unix milliseconds.
    pub expiry_unix_ms: u64,
    /// CAS revision.
    pub revision: Revision,
    /// Bound run identity.
    pub run_id: String,
    /// Bound attempt identity.
    pub attempt_id: String,
    /// Bound specification MAC hex.
    pub spec_mac_hex: String,
}

impl AttemptLease {
    /// Fail closed when the caller is not the current owner/epoch.
    pub fn require_holder(&self, owner: &str, epoch: u64) -> Result<(), SpineError> {
        if self.owner != owner || self.epoch != epoch {
            return Err(SpineError::StaleRevision);
        }
        Ok(())
    }

    /// Fail closed when `now` is at or after expiry.
    pub fn require_unexpired(&self, now_unix_ms: u64) -> Result<(), SpineError> {
        if now_unix_ms >= self.expiry_unix_ms {
            return Err(SpineError::StaleRevision);
        }
        Ok(())
    }
}

/// Compare-and-swap fence for a lease mutation.
pub fn cas_lease(
    current: &AttemptLease,
    expected_revision: Revision,
    expected_owner: &str,
    expected_epoch: u64,
    next: AttemptLease,
) -> Result<AttemptLease, SpineError> {
    current.revision.require_current(expected_revision)?;
    current.require_holder(expected_owner, expected_epoch)?;
    if next.lease_id != current.lease_id
        || next.run_id != current.run_id
        || next.attempt_id != current.attempt_id
        || next.spec_mac_hex != current.spec_mac_hex
    {
        return Err(SpineError::CrossScope);
    }
    if next.revision != current.revision.checked_next()? {
        return Err(SpineError::StaleRevision);
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AttemptLease {
        AttemptLease {
            lease_id: "lease-1".into(),
            owner: "owner-a".into(),
            epoch: 1,
            expiry_unix_ms: 2_000,
            revision: Revision::new(1),
            run_id: "run-1".into(),
            attempt_id: "att-1".into(),
            spec_mac_hex: "ab".repeat(32),
        }
    }

    #[test]
    fn stale_owner_and_expiry_fail_closed() {
        let lease = sample();
        assert_eq!(
            lease.require_holder("owner-b", 1),
            Err(SpineError::StaleRevision)
        );
        assert_eq!(
            lease.require_unexpired(2_000),
            Err(SpineError::StaleRevision)
        );
        lease.require_unexpired(1_999).unwrap();
    }

    #[test]
    fn second_agent_cannot_cas_foreign_lease() {
        let current = sample();
        let mut next = current.clone();
        next.owner = "owner-b".into();
        next.revision = Revision::new(2);
        assert_eq!(
            cas_lease(&current, Revision::new(1), "owner-b", 1, next),
            Err(SpineError::StaleRevision)
        );
    }
}
