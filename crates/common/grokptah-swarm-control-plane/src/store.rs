//! Durable persistence and compare-and-swap seams.
//!
//! The controller owns a value, not a database. A real owner implements
//! [`DurableSwarmStore`] with an atomic durable transaction. The transaction
//! must persist the next swarm revision and, when present, consume the
//! Computer Use lease claim in the same critical section. That global lease
//! claim is what prevents one externally issued grant from being copied into
//! two different swarms.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::error::{SwarmError, SwarmResult};
use crate::ids::{DispatchId, LeaseId, SwarmId, TaskId};
use crate::spec::ComputerUseLeaseRef;
use crate::state::{DispatchRecord, SwarmState};

/// The immutable identity a durable store consumes for one Computer Use grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseClaim {
    pub lease_id: LeaseId,
    pub swarm_id: SwarmId,
    pub task_id: TaskId,
    pub dispatch_id: DispatchId,
}

impl LeaseClaim {
    pub(crate) fn from_dispatch(
        swarm_id: &SwarmId,
        dispatch: &DispatchRecord,
        lease: &ComputerUseLeaseRef,
    ) -> Self {
        Self {
            lease_id: lease.lease_id.clone(),
            swarm_id: swarm_id.clone(),
            task_id: dispatch.task_id.clone(),
            dispatch_id: dispatch.dispatch_id.clone(),
        }
    }
}

/// Durable owner-side state store.
///
/// `compare_and_swap` is the side-effect fence: implementations must atomically
/// verify `expected_revision`, persist `next`, and consume `lease_claim` in a
/// global lease table. A filesystem or hosted implementation must not split
/// the lease claim from the state write. Repeating the exact same claim is
/// idempotent; any other use of the lease ID must fail without mutation.
pub trait DurableSwarmStore: Send + Sync + std::fmt::Debug {
    /// Create a new swarm record at its initial revision.
    fn create(&self, state: &SwarmState) -> SwarmResult<()>;

    /// Read the latest durable record for one swarm.
    fn load(&self, swarm_id: &SwarmId) -> SwarmResult<SwarmState>;

    /// Atomically persist a revision and, optionally, consume one lease.
    fn compare_and_swap(
        &self,
        swarm_id: &SwarmId,
        expected_revision: u64,
        next: &SwarmState,
        lease_claim: Option<&LeaseClaim>,
    ) -> SwarmResult<()>;
}

#[derive(Debug, Default)]
struct MemoryStoreState {
    swarms: BTreeMap<SwarmId, SwarmState>,
    consumed_leases: BTreeMap<LeaseId, LeaseClaim>,
}

/// Deterministic reference store for tests and offline adapters.
///
/// This is intentionally not a production durability implementation. It
/// provides the same revision and cross-swarm lease rules that a file or
/// hosted store must provide, making those rules testable without providers.
#[derive(Debug, Clone, Default)]
pub struct InMemorySwarmStore {
    inner: Arc<Mutex<MemoryStoreState>>,
}

impl InMemorySwarmStore {
    fn lock(&self) -> SwarmResult<std::sync::MutexGuard<'_, MemoryStoreState>> {
        self.inner.lock().map_err(|_| {
            SwarmError::new(
                crate::SwarmErrorCode::CorruptState,
                "swarm store lock poisoned",
            )
        })
    }

    fn claim_for_dispatch(swarm_id: &SwarmId, dispatch: &DispatchRecord) -> Option<LeaseClaim> {
        dispatch
            .lease
            .as_ref()
            .map(|lease| LeaseClaim::from_dispatch(swarm_id, dispatch, lease))
    }

    fn check_claim(
        consumed_leases: &mut BTreeMap<LeaseId, LeaseClaim>,
        claim: &LeaseClaim,
    ) -> SwarmResult<()> {
        if let Some(existing) = consumed_leases.get(&claim.lease_id) {
            if existing == claim {
                return Ok(());
            }
            return Err(SwarmError::capability(
                "Computer Use lease has already been consumed by another dispatch",
            ));
        }
        consumed_leases.insert(claim.lease_id.clone(), claim.clone());
        Ok(())
    }
}

impl DurableSwarmStore for InMemorySwarmStore {
    fn create(&self, state: &SwarmState) -> SwarmResult<()> {
        let mut store = self.lock()?;
        if store.swarms.contains_key(&state.spec.swarm_id) {
            return Err(SwarmError::conflict(
                "swarm already exists in the durable store",
            ));
        }
        for dispatch in &state.dispatches {
            if let Some(claim) = Self::claim_for_dispatch(&state.spec.swarm_id, dispatch) {
                Self::check_claim(&mut store.consumed_leases, &claim)?;
            }
        }
        store
            .swarms
            .insert(state.spec.swarm_id.clone(), state.clone());
        Ok(())
    }

    fn load(&self, swarm_id: &SwarmId) -> SwarmResult<SwarmState> {
        self.lock()?
            .swarms
            .get(swarm_id)
            .cloned()
            .ok_or_else(|| SwarmError::not_found("swarm is not present in the durable store"))
    }

    fn compare_and_swap(
        &self,
        swarm_id: &SwarmId,
        expected_revision: u64,
        next: &SwarmState,
        lease_claim: Option<&LeaseClaim>,
    ) -> SwarmResult<()> {
        let mut store = self.lock()?;
        let current =
            store.swarms.get(swarm_id).cloned().ok_or_else(|| {
                SwarmError::not_found("swarm is not present in the durable store")
            })?;
        if current.revision != expected_revision {
            return Err(SwarmError::conflict(
                "swarm revision is stale; reload before retrying the mutation",
            ));
        }
        if next.spec.swarm_id != *swarm_id || next.revision != expected_revision.saturating_add(1) {
            return Err(SwarmError::conflict(
                "durable state does not advance the expected swarm revision",
            ));
        }

        let current_dispatches: BTreeMap<&DispatchId, &DispatchRecord> = current
            .dispatches
            .iter()
            .map(|dispatch| (&dispatch.dispatch_id, dispatch))
            .collect();
        for dispatch in &next.dispatches {
            if let Some(previous) = current_dispatches.get(&dispatch.dispatch_id) {
                if previous.lease != dispatch.lease {
                    return Err(SwarmError::corrupt(
                        "a dispatch lease is immutable after its first durable write",
                    ));
                }
                continue;
            }
            let Some(claim) = Self::claim_for_dispatch(swarm_id, dispatch) else {
                continue;
            };
            if lease_claim != Some(&claim) {
                return Err(SwarmError::corrupt(
                    "a new Computer Use dispatch is missing its durable lease claim",
                ));
            }
            Self::check_claim(&mut store.consumed_leases, &claim)?;
        }

        store.swarms.insert(swarm_id.clone(), next.clone());
        Ok(())
    }
}
