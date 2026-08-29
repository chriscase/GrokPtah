//! Durable work claims and revisions.
//!
//! One work item, one revision counter, one holder. Every mutation is a
//! compare-and-set against the revision the caller last saw, so a worker acting
//! on a stale view is refused rather than silently overwriting a newer decision.
//!
//! This is the ownership half of the durable Work ledger that already exists on
//! `main`; it does not introduce a second Work type, scheduler, or lease
//! universe.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Monotonic revision of one work item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Who holds a claim, and until when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimHolder {
    pub worker_id: String,
    /// Monotonic host time in milliseconds at which the lease expires.
    pub lease_expires_at_ms: u64,
    /// Incremented each time this worker re-establishes the same claim, so a
    /// duplicate process is visible rather than merely tolerated.
    pub reclaims: u32,
}

/// One claimable work item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRecord {
    pub work_id: String,
    pub revision: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<ClaimHolder>,
    #[serde(default)]
    pub completed: bool,
}

impl ClaimRecord {
    pub fn unclaimed(work_id: impl Into<String>) -> Self {
        Self {
            work_id: work_id.into(),
            revision: Revision(1),
            holder: None,
            completed: false,
        }
    }

    /// Whether the lease is live at `now_ms`.
    pub fn is_leased_at(&self, now_ms: u64) -> bool {
        self.holder
            .as_ref()
            .is_some_and(|h| h.lease_expires_at_ms > now_ms)
    }
}

/// Why a claim was refused. Every variant leaves the ledger unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// The caller's revision is not the current one.
    StaleRevision {
        expected: Revision,
        actual: Revision,
    },
    /// A different worker holds a live lease.
    HeldByAnother { worker_id: String },
    /// No such work item.
    Unknown { work_id: String },
    /// The item is already finished.
    AlreadyCompleted,
    /// The caller does not hold the lease it is trying to act on.
    NotHolder,
}

impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRevision { expected, actual } => {
                write!(f, "stale revision {expected}; current is {actual}")
            }
            // Deliberately does not name the holder: a refusal must not become
            // an oracle for who else is working in this home.
            Self::HeldByAnother { .. } => f.write_str("work item is claimed"),
            Self::Unknown { .. } => f.write_str("work item is claimed"),
            Self::AlreadyCompleted => f.write_str("work item is already completed"),
            Self::NotHolder => f.write_str("caller does not hold this lease"),
        }
    }
}

impl std::error::Error for ClaimError {}

/// The outcome of a successful claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimed {
    pub work_id: String,
    pub revision: Revision,
    pub worker_id: String,
    /// True when this call re-established a claim the same worker already held.
    pub idempotent: bool,
}

/// Compare-and-set claim ledger.
#[derive(Debug, Default)]
pub struct ClaimLedger {
    items: BTreeMap<String, ClaimRecord>,
}

impl ClaimLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from surviving records after a restart.
    pub fn recover(records: impl IntoIterator<Item = ClaimRecord>) -> Self {
        Self {
            items: records
                .into_iter()
                .map(|r| (r.work_id.clone(), r))
                .collect(),
        }
    }

    pub fn insert(&mut self, record: ClaimRecord) {
        self.items.insert(record.work_id.clone(), record);
    }

    pub fn get(&self, work_id: &str) -> Option<&ClaimRecord> {
        self.items.get(work_id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Claim a work item, compare-and-set against `expected`.
    ///
    /// Re-claiming an item this same worker already holds is idempotent: a
    /// process that crashed after writing its claim but before recording the
    /// fact must be able to resume without a second lease appearing.
    /// A *different* worker is refused while the lease is live, which is what
    /// makes a duplicate worker safe rather than merely unlikely.
    pub fn claim(
        &mut self,
        work_id: &str,
        worker_id: &str,
        expected: Revision,
        now_ms: u64,
        lease_ms: u64,
    ) -> Result<Claimed, ClaimError> {
        let record = self
            .items
            .get_mut(work_id)
            .ok_or_else(|| ClaimError::Unknown {
                work_id: work_id.to_string(),
            })?;
        if record.completed {
            return Err(ClaimError::AlreadyCompleted);
        }
        if record.revision != expected {
            return Err(ClaimError::StaleRevision {
                expected,
                actual: record.revision,
            });
        }
        let idempotent = match record.holder.as_ref() {
            Some(holder) if holder.worker_id == worker_id => true,
            Some(holder) if holder.lease_expires_at_ms > now_ms => {
                return Err(ClaimError::HeldByAnother {
                    worker_id: holder.worker_id.clone(),
                });
            }
            // An expired lease is reclaimable by anyone; that is how a crashed
            // worker's work returns to the pool.
            Some(_) | None => false,
        };
        let reclaims = record
            .holder
            .as_ref()
            .filter(|h| h.worker_id == worker_id)
            .map_or(0, |h| h.reclaims.saturating_add(1));
        record.holder = Some(ClaimHolder {
            worker_id: worker_id.to_string(),
            lease_expires_at_ms: now_ms.saturating_add(lease_ms),
            reclaims,
        });
        record.revision = record.revision.next();
        Ok(Claimed {
            work_id: work_id.to_string(),
            revision: record.revision,
            worker_id: worker_id.to_string(),
            idempotent,
        })
    }

    /// Extend a live lease. Only the holder may.
    pub fn heartbeat(
        &mut self,
        work_id: &str,
        worker_id: &str,
        now_ms: u64,
        lease_ms: u64,
    ) -> Result<Revision, ClaimError> {
        let record = self
            .items
            .get_mut(work_id)
            .ok_or_else(|| ClaimError::Unknown {
                work_id: work_id.to_string(),
            })?;
        match record.holder.as_mut() {
            Some(holder) if holder.worker_id == worker_id => {
                holder.lease_expires_at_ms = now_ms.saturating_add(lease_ms);
                Ok(record.revision)
            }
            _ => Err(ClaimError::NotHolder),
        }
    }

    /// Release a claim without completing the work.
    pub fn release(
        &mut self,
        work_id: &str,
        worker_id: &str,
        expected: Revision,
    ) -> Result<Revision, ClaimError> {
        let record = self
            .items
            .get_mut(work_id)
            .ok_or_else(|| ClaimError::Unknown {
                work_id: work_id.to_string(),
            })?;
        if record.revision != expected {
            return Err(ClaimError::StaleRevision {
                expected,
                actual: record.revision,
            });
        }
        match record.holder.as_ref() {
            Some(holder) if holder.worker_id == worker_id => {
                record.holder = None;
                record.revision = record.revision.next();
                Ok(record.revision)
            }
            _ => Err(ClaimError::NotHolder),
        }
    }

    /// Complete the work. Only the holder may, and only on a current revision.
    pub fn complete(
        &mut self,
        work_id: &str,
        worker_id: &str,
        expected: Revision,
    ) -> Result<Revision, ClaimError> {
        let record = self
            .items
            .get_mut(work_id)
            .ok_or_else(|| ClaimError::Unknown {
                work_id: work_id.to_string(),
            })?;
        if record.completed {
            return Err(ClaimError::AlreadyCompleted);
        }
        if record.revision != expected {
            return Err(ClaimError::StaleRevision {
                expected,
                actual: record.revision,
            });
        }
        match record.holder.as_ref() {
            Some(holder) if holder.worker_id == worker_id => {
                record.completed = true;
                record.holder = None;
                record.revision = record.revision.next();
                Ok(record.revision)
            }
            _ => Err(ClaimError::NotHolder),
        }
    }

    /// Work whose lease has expired and which is therefore reclaimable.
    pub fn expired(&self, now_ms: u64) -> Vec<&ClaimRecord> {
        self.items
            .values()
            .filter(|r| !r.completed && r.holder.is_some() && !r.is_leased_at(now_ms))
            .collect()
    }
}
