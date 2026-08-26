//! Closed registration and supervision primitive.
//!
//! Owns cancel, abort, bounded await, quiescence proof, and capacity release.
//! [`Registration`] Drop may record cleanup-required but cannot release
//! capacity or claim worker death. A process-local boolean is not a lease;
//! the durable [`super::lease::AttemptLease`] remains authoritative.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::authority::SpineError;

/// Proof that a registration is quiescent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuiescenceProof {
    /// Registration identity.
    pub id: String,
    /// True when owned tasks are no longer live.
    pub quiescent: bool,
}

struct Slot {
    cancelled: bool,
    aborted: bool,
    live: bool,
    cleanup_required: bool,
    capacity_released: bool,
    worker_death_claimed: bool,
    gate_open: bool,
    cancel: CancellationToken,
    worker: Option<JoinHandle<()>>,
    aggregator: Option<JoinHandle<()>>,
    supervisor: Option<JoinHandle<()>>,
}

struct SupervisorInner {
    capacity: u32,
    used: u32,
    slots: BTreeMap<String, Slot>,
}

/// Supervisor owning a bounded set of registrations.
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Mutex<SupervisorInner>>,
}

impl Supervisor {
    /// Create with a fixed capacity.
    pub fn new(capacity: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SupervisorInner {
                capacity,
                used: 0,
                slots: BTreeMap::new(),
            })),
        }
    }

    /// Atomically register worker, aggregator, and supervisor handles behind a
    /// closed start gate. No task is considered started until [`Registration::open_gate`].
    pub fn register_closed(
        &self,
        id: impl Into<String>,
        worker: JoinHandle<()>,
        aggregator: JoinHandle<()>,
        supervisor: JoinHandle<()>,
        cancel: CancellationToken,
    ) -> Result<Registration, SpineError> {
        let id = id.into();
        let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        if inner.used >= self_capacity(&inner) {
            worker.abort();
            aggregator.abort();
            supervisor.abort();
            return Err(SpineError::Capacity);
        }
        if inner.slots.contains_key(&id) {
            worker.abort();
            aggregator.abort();
            supervisor.abort();
            return Err(SpineError::DuplicateIdentity);
        }
        inner.used = inner
            .used
            .checked_add(1)
            .ok_or(SpineError::RevisionOverflow)?;
        inner.slots.insert(
            id.clone(),
            Slot {
                cancelled: false,
                aborted: false,
                live: true,
                cleanup_required: false,
                capacity_released: false,
                worker_death_claimed: false,
                gate_open: false,
                cancel,
                worker: Some(worker),
                aggregator: Some(aggregator),
                supervisor: Some(supervisor),
            },
        );
        Ok(Registration {
            id,
            inner: Arc::clone(&self.inner),
        })
    }

    /// Used slots.
    pub fn used(&self) -> u32 {
        self.inner
            .lock()
            .map(|inner| inner.used)
            .unwrap_or(u32::MAX)
    }

    /// Abort by id after the handle is gone. Does not release capacity.
    pub fn abort_id(&self, id: &str) -> Result<(), SpineError> {
        let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        let slot = inner.slots.get_mut(id).ok_or(SpineError::InvalidIdentity)?;
        slot.aborted = true;
        slot.cancelled = true;
        slot.live = false;
        slot.cancel.cancel();
        abort_handles(slot);
        slot.worker.take();
        slot.aggregator.take();
        slot.supervisor.take();
        Ok(())
    }

    /// Release capacity only after abort and quiescence. Not callable from Drop.
    pub fn release_capacity(&self, id: &str) -> Result<(), SpineError> {
        let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        let slot = inner.slots.get_mut(id).ok_or(SpineError::InvalidIdentity)?;
        if !slot.aborted || slot.live {
            return Err(SpineError::Capacity);
        }
        if slot.worker.is_some() || slot.aggregator.is_some() || slot.supervisor.is_some() {
            return Err(SpineError::Capacity);
        }
        if slot.capacity_released {
            return Err(SpineError::DuplicateIdentity);
        }
        slot.capacity_released = true;
        inner.used = inner.used.saturating_sub(1);
        Ok(())
    }

    /// Snapshot whether Drop claimed death or released capacity (it must not).
    pub fn slot_flags(&self, id: &str) -> Result<(bool, bool, bool), SpineError> {
        let inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        let slot = inner.slots.get(id).ok_or(SpineError::InvalidIdentity)?;
        Ok((
            slot.cleanup_required,
            slot.capacity_released,
            slot.worker_death_claimed,
        ))
    }
}

fn self_capacity(inner: &SupervisorInner) -> u32 {
    inner.capacity
}

fn abort_handles(slot: &mut Slot) {
    if let Some(handle) = slot.worker.as_ref() {
        handle.abort();
    }
    if let Some(handle) = slot.aggregator.as_ref() {
        handle.abort();
    }
    if let Some(handle) = slot.supervisor.as_ref() {
        handle.abort();
    }
}

/// Live registration handle.
pub struct Registration {
    id: String,
    inner: Arc<Mutex<SupervisorInner>>,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("id", &self.id)
            .finish()
    }
}

impl Registration {
    /// Registration identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Open the start gate after Starting is persisted.
    pub fn open_gate(&self) -> Result<(), SpineError> {
        let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        let slot = inner
            .slots
            .get_mut(&self.id)
            .ok_or(SpineError::InvalidIdentity)?;
        if slot.cancelled || slot.aborted {
            return Err(SpineError::TransitionForbidden);
        }
        slot.gate_open = true;
        Ok(())
    }

    /// Whether the gate is open.
    pub fn gate_is_open(&self) -> Result<bool, SpineError> {
        let inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        Ok(inner
            .slots
            .get(&self.id)
            .ok_or(SpineError::InvalidIdentity)?
            .gate_open)
    }

    /// Cooperative cancel.
    pub fn cancel(&self) -> Result<(), SpineError> {
        let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        let slot = inner
            .slots
            .get_mut(&self.id)
            .ok_or(SpineError::InvalidIdentity)?;
        slot.cancelled = true;
        slot.cancel.cancel();
        Ok(())
    }

    /// Abort the slot and mark it not live. Does not release capacity.
    pub fn abort(&self) -> Result<(), SpineError> {
        let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
        let slot = inner
            .slots
            .get_mut(&self.id)
            .ok_or(SpineError::InvalidIdentity)?;
        slot.aborted = true;
        slot.cancelled = true;
        slot.live = false;
        slot.cancel.cancel();
        abort_handles(slot);
        Ok(())
    }

    /// Bounded await for quiescence after abort. Joins nested tasks.
    pub async fn wait_quiescent(&self, budget: Duration) -> Result<QuiescenceProof, SpineError> {
        let deadline = Instant::now() + budget;
        let (worker, aggregator, supervisor) = {
            let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
            let slot = inner
                .slots
                .get_mut(&self.id)
                .ok_or(SpineError::InvalidIdentity)?;
            (
                slot.worker.take(),
                slot.aggregator.take(),
                slot.supervisor.take(),
            )
        };
        for handle in [worker, aggregator, supervisor].into_iter().flatten() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                handle.abort();
                return Err(SpineError::Capacity);
            }
            match tokio::time::timeout(remaining, handle).await {
                Ok(_) => {}
                Err(_) => return Err(SpineError::Capacity),
            }
        }
        {
            let mut inner = self.inner.lock().map_err(|_| SpineError::Capacity)?;
            if let Some(slot) = inner.slots.get_mut(&self.id) {
                slot.live = false;
            }
        }
        Ok(QuiescenceProof {
            id: self.id.clone(),
            quiescent: true,
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(slot) = inner.slots.get_mut(&self.id) {
                slot.cleanup_required = true;
                // Must not release capacity or claim worker death.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drop_does_not_release_capacity_or_claim_death() {
        let supervisor = Supervisor::new(1);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(async {});
        let aggregator = tokio::spawn(async {});
        let supervisor_task = tokio::spawn(async {});
        let id;
        {
            let reg = supervisor
                .register_closed("worker-1", worker, aggregator, supervisor_task, cancel)
                .unwrap();
            id = reg.id().to_string();
            assert_eq!(supervisor.used(), 1);
            drop(reg);
        }
        let (cleanup, released, death) = supervisor.slot_flags(&id).unwrap();
        assert!(cleanup);
        assert!(!released);
        assert!(!death);
        assert_eq!(supervisor.used(), 1);
        supervisor.abort_id(&id).unwrap();
        // Handles were aborted; join them via a fresh registration wait is
        // not possible. Capacity remains until explicit release after abort
        // and empty handles.
        supervisor.release_capacity(&id).unwrap();
        assert_eq!(supervisor.used(), 0);
    }

    #[tokio::test]
    async fn second_registration_is_capacity_denied() {
        let supervisor = Supervisor::new(1);
        let reg = supervisor
            .register_closed(
                "a",
                tokio::spawn(async {}),
                tokio::spawn(async {}),
                tokio::spawn(async {}),
                CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            supervisor
                .register_closed(
                    "b",
                    tokio::spawn(async {}),
                    tokio::spawn(async {}),
                    tokio::spawn(async {}),
                    CancellationToken::new(),
                )
                .unwrap_err(),
            SpineError::Capacity
        );
        drop(reg);
    }
}
