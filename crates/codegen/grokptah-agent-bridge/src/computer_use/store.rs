use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use uuid::Uuid;

use super::coordination::{
    conflict_domain_id, invalid_lease_record, validate_payload_digest, ComputerDispatchClaim,
    ComputerDispatchRecord, ComputerDispatchState, ComputerSurfaceLease, ComputerSurfaceLeaseState,
    HostSurfaceLeaseRequest, COMPUTER_DISPATCH_SCHEMA_VERSION,
    COMPUTER_SURFACE_LEASE_SCHEMA_VERSION, MAX_SURFACE_LEASES,
};
use super::types::{
    validate_id, validate_workspace, ComputerControlDisposition, ComputerError, ComputerErrorCode,
    ComputerPrincipal, ComputerResult, ComputerRun, ComputerRunState, ComputerSurfaceBinding,
    PhysicalInputDomain, SurfaceFreshnessFence, COMPUTER_RECEIPT_SCHEMA_VERSION,
    COMPUTER_RUN_SCHEMA_VERSION,
};

pub(crate) const MAX_RECEIPTS: usize = 2_048;
const MAX_RECORD_BYTES: u64 = 32 * 1024 * 1024;
const TERMINAL_RUN_AGE: Duration = Duration::days(30);
const TERMINAL_RECEIPT_AGE: Duration = Duration::days(7);
const TERMINAL_SURFACE_LEASE_AGE: Duration = Duration::days(7);

#[derive(Clone)]
pub struct ComputerStore {
    inner: Arc<ComputerStoreInner>,
}

struct ComputerStoreInner {
    root: PathBuf,
    _store_lock: fs::File,
    lock: Mutex<()>,
    surfaces: Mutex<SurfaceRegistry>,
}

#[derive(Default)]
struct SurfaceRegistry {
    by_domain: HashMap<String, Arc<LiveSurfaceState>>,
    by_surface_id: HashMap<String, Arc<LiveSurfaceState>>,
}

struct LiveSurfaceState {
    binding: ComputerSurfaceBinding,
    input_domain_id: String,
    conflict_domain_id: String,
    measurement_id: String,
    tick: AtomicU64,
    frame_epoch: AtomicU64,
}

/// Host-interned surface for one attested physical input domain.
#[derive(Debug, Clone)]
pub(crate) struct InternedSurface {
    pub binding: ComputerSurfaceBinding,
    pub input_domain_id: String,
    pub measurement_id: String,
}

impl InternedSurface {
    pub(crate) fn stamp_proof(
        &self,
        proof: crate::computer_use::ComputerCapabilityProof,
    ) -> ComputerResult<crate::computer_use::ComputerCapabilityProof> {
        proof.bind_to_interned_surface(&self.binding, &self.input_domain_id, &self.measurement_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptState {
    Claimed,
    Succeeded,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MutationReceipt {
    request_id: String,
    operation: String,
    payload_hash: String,
    state: ReceiptState,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    result: Option<serde_json::Value>,
    error: Option<ComputerError>,
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    caller_kind: Option<String>,
    #[serde(default)]
    caller_owner_session_id: Option<Uuid>,
    #[serde(default)]
    caller_agent_id: Option<String>,
    #[serde(default)]
    caller_agent_spec_revision: Option<u64>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    surface_id: Option<String>,
    #[serde(default)]
    incarnation: Option<String>,
    #[serde(default)]
    grant_id: Option<String>,
    #[serde(default)]
    frame_epoch: Option<u64>,
    #[serde(default, alias = "authorityEpoch")]
    pre_authority_epoch: Option<u64>,
    #[serde(default, alias = "controlEpoch")]
    pre_control_epoch: Option<u64>,
    #[serde(default)]
    post_authority_epoch: Option<u64>,
    #[serde(default)]
    post_control_epoch: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct MutationStamp {
    pub principal: ComputerPrincipal,
    pub run_id: Option<String>,
    pub surface_id: Option<String>,
    pub incarnation: Option<String>,
    pub grant_id: Option<String>,
    pub frame_epoch: Option<u64>,
    pub pre_authority_epoch: u64,
    pub pre_control_epoch: u64,
}

impl MutationStamp {
    pub(crate) fn from_caller(principal: ComputerPrincipal, run: Option<&ComputerRun>) -> Self {
        Self {
            principal,
            run_id: run.map(|run| run.run_id.clone()),
            surface_id: run.and_then(|run| {
                run.surface
                    .is_issued()
                    .then(|| run.surface.surface_id.clone())
            }),
            incarnation: run.and_then(|run| {
                run.surface
                    .is_issued()
                    .then(|| run.surface.incarnation.clone())
            }),
            grant_id: run.and_then(|run| run.grant.as_ref().map(|grant| grant.grant_id.clone())),
            frame_epoch: run.and_then(|run| {
                run.current_observation
                    .as_ref()
                    .map(|observation| observation.authority.frame_epoch)
            }),
            pre_authority_epoch: run.map(|run| run.authority_epoch).unwrap_or(0),
            pre_control_epoch: run.map(|run| run.control_epoch).unwrap_or(0),
        }
    }

    fn kind(&self) -> &'static str {
        self.principal.public_kind()
    }

    fn owner_session_id(&self) -> Option<Uuid> {
        self.principal.session_id()
    }

    fn agent_id(&self) -> Option<String> {
        self.principal.agent_id().map(str::to_string)
    }

    fn agent_spec_revision(&self) -> Option<u64> {
        self.principal.agent_spec_revision()
    }
}

#[derive(Debug)]
pub(crate) enum MutationClaim {
    Perform,
    Pending,
    Uncertain,
    Replay(ComputerResult<serde_json::Value>),
}

impl ComputerStore {
    /// Durable ledger ceiling, surfaced so capacity reads report the same
    /// bound the store actually enforces in [`Self::can_create_run`].
    pub const MAX_RUN_RECORDS: usize = 256;

    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("runs"))?;
        fs::create_dir_all(root.join("receipts"))?;
        fs::create_dir_all(root.join("surface-leases"))?;
        let root = dunce::canonicalize(root)?;
        let lock_path = root.join(".store.lock");
        let store_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        store_lock.try_lock_exclusive().map_err(|error| {
            anyhow::anyhow!(
                "computer-use store {} is already open ({error})",
                root.display()
            )
        })?;
        let store = Self {
            inner: Arc::new(ComputerStoreInner {
                root,
                _store_lock: store_lock,
                lock: Mutex::new(()),
                surfaces: Mutex::new(SurfaceRegistry::default()),
            }),
        };
        store.validate_surface_lease_records()?;
        store.recover_interrupted()?;
        store.recover_surface_leases()?;
        store.recover_receipts()?;
        store.prune_retention()?;
        Ok(store)
    }

    fn validate_surface_lease_records(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        self.list_surface_leases_unlocked().map(|_| ())
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.inner.root
    }

    pub(crate) fn save_run(&self, run: &ComputerRun) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        validate_run_record(run)?;
        let path = self.run_path(&run.run_id)?;
        atomic_write_json(&path, run).map_err(internal_error)
    }

    pub(crate) fn load_run(&self, run_id: &str) -> ComputerResult<Option<ComputerRun>> {
        let _guard = self.inner.lock.lock();
        self.load_run_unlocked(run_id)
    }

    pub(crate) fn update_run<F>(
        &self,
        run_id: &str,
        update: F,
    ) -> ComputerResult<Option<ComputerRun>>
    where
        F: FnOnce(&mut ComputerRun) -> ComputerResult<()>,
    {
        let _guard = self.inner.lock.lock();
        let Some(mut run) = self.load_run_unlocked(run_id)? else {
            return Ok(None);
        };
        update(&mut run)?;
        validate_run_record(&run)?;
        atomic_write_json(&self.run_path(run_id)?, &run).map_err(internal_error)?;
        Ok(Some(run))
    }

    /// Apply an out-of-band control transition and revoke the Run's active
    /// surface leases while holding the same host linearization lock used by
    /// dispatch preparation/injection. If injection won the lock first, its
    /// lease becomes Uncertain; otherwise it becomes KnownNotInjected.
    pub(crate) fn update_run_and_revoke_surface_leases<F>(
        &self,
        run_id: &str,
        disposition: &str,
        now: DateTime<Utc>,
        update: F,
    ) -> ComputerResult<Option<ComputerRun>>
    where
        F: FnOnce(&mut ComputerRun) -> ComputerResult<()>,
    {
        validate_id("run_id", run_id)?;
        validate_id("disposition", disposition)?;
        let _guard = self.inner.lock.lock();
        let Some(mut run) = self.load_run_unlocked(run_id)? else {
            return Ok(None);
        };
        update(&mut run)?;
        validate_run_record(&run)?;
        atomic_write_json(&self.run_path(run_id)?, &run).map_err(internal_error)?;

        for mut lease in self
            .list_surface_leases_unlocked()?
            .into_iter()
            .filter(|lease| lease.run_id == run_id && !lease.state.is_terminal())
        {
            match lease.state {
                ComputerSurfaceLeaseState::Queued | ComputerSurfaceLeaseState::Granted => {
                    lease.transition(ComputerSurfaceLeaseState::Revoked, now, Some(disposition))?;
                }
                ComputerSurfaceLeaseState::Dispatching => {
                    let dispatch = lease.dispatch.as_mut().ok_or_else(invalid_lease_record)?;
                    match dispatch.state {
                        ComputerDispatchState::Prepared => {
                            dispatch.state = ComputerDispatchState::KnownNotInjected;
                            dispatch.completed_at = Some(now);
                            dispatch.error_code = Some(ComputerErrorCode::PermissionRevoked);
                            lease.transition(
                                ComputerSurfaceLeaseState::Revoked,
                                now,
                                Some(disposition),
                            )?;
                        }
                        ComputerDispatchState::Injected => {
                            dispatch.state = ComputerDispatchState::Uncertain;
                            dispatch.completed_at = Some(now);
                            dispatch.error_code = Some(ComputerErrorCode::UncertainOutcome);
                            lease.transition(
                                ComputerSurfaceLeaseState::Uncertain,
                                now,
                                Some(disposition),
                            )?;
                        }
                        _ => return Err(invalid_lease_record()),
                    }
                }
                _ => return Err(invalid_lease_record()),
            }
            self.write_surface_lease_unlocked(&lease)?;
        }
        Ok(Some(run))
    }

    pub(crate) fn list_runs(&self) -> ComputerResult<Vec<ComputerRun>> {
        let _guard = self.inner.lock.lock();
        self.list_runs_unlocked()
    }

    pub(crate) fn can_create_run(&self) -> ComputerResult<()> {
        let count = self.list_runs()?.len();
        if count >= Self::MAX_RUN_RECORDS {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "computer-use run record limit reached",
            ));
        }
        Ok(())
    }

    pub(crate) fn intern_physical_domain(
        &self,
        domain: &PhysicalInputDomain,
    ) -> ComputerResult<InternedSurface> {
        let mut registry = self.inner.surfaces.lock();
        if let Some(existing) = registry.by_domain.get(domain.as_key()) {
            return Ok(InternedSurface {
                binding: existing.binding.clone(),
                input_domain_id: existing.input_domain_id.clone(),
                measurement_id: existing.measurement_id.clone(),
            });
        }
        let binding = ComputerSurfaceBinding::issue();
        let conflict_domain_id = conflict_domain_id(domain.as_key());
        let state = Arc::new(LiveSurfaceState {
            binding: binding.clone(),
            input_domain_id: Uuid::new_v4().to_string(),
            conflict_domain_id,
            measurement_id: Uuid::new_v4().to_string(),
            tick: AtomicU64::new(0),
            frame_epoch: AtomicU64::new(0),
        });
        registry
            .by_domain
            .insert(domain.as_key().to_string(), state.clone());
        registry
            .by_surface_id
            .insert(binding.surface_id.clone(), state.clone());
        Ok(InternedSurface {
            binding,
            input_domain_id: state.input_domain_id.clone(),
            measurement_id: state.measurement_id.clone(),
        })
    }

    pub(crate) fn live_freshness(
        &self,
        binding: &ComputerSurfaceBinding,
    ) -> ComputerResult<SurfaceFreshnessFence> {
        binding.validate()?;
        let registry = self.inner.surfaces.lock();
        let live = registry
            .by_surface_id
            .get(&binding.surface_id)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "surface was not interned by the host registry",
                )
            })?;
        if live.binding.incarnation != binding.incarnation {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "surface incarnation was invalidated by restart",
            ));
        }
        Ok(SurfaceFreshnessFence {
            surface_id: binding.surface_id.clone(),
            incarnation: binding.incarnation.clone(),
            tick: live.tick.load(Ordering::SeqCst),
            wall_clock: Some(Utc::now()),
        })
    }

    pub(crate) fn mint_observation_clock(
        &self,
        binding: &ComputerSurfaceBinding,
    ) -> ComputerResult<(SurfaceFreshnessFence, u64)> {
        binding.validate()?;
        let registry = self.inner.surfaces.lock();
        let live = registry
            .by_surface_id
            .get(&binding.surface_id)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "surface was not interned by the host registry",
                )
            })?;
        if live.binding.incarnation != binding.incarnation {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "surface incarnation was invalidated by restart",
            ));
        }
        let previous_tick = live.tick.load(Ordering::SeqCst);
        let previous_frame = live.frame_epoch.load(Ordering::SeqCst);
        let tick = live.tick.fetch_add(1, Ordering::SeqCst) + 1;
        let frame_epoch = live.frame_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        if tick <= previous_tick || frame_epoch <= previous_frame {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "host surface registry clocks are not monotonic",
            ));
        }
        Ok((
            SurfaceFreshnessFence {
                surface_id: binding.surface_id.clone(),
                incarnation: binding.incarnation.clone(),
                tick,
                wall_clock: Some(Utc::now()),
            },
            frame_epoch,
        ))
    }

    /// Queue one WorkAttempt on the conflict domain derived from the Run's
    /// host-interned surface. Caller/backend data never supplies the domain,
    /// queue sequence, surface, principal, or epoch fences.
    pub(crate) fn queue_surface_lease(
        &self,
        request: HostSurfaceLeaseRequest,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerSurfaceLease> {
        request.validate(now)?;
        let _guard = self.inner.lock.lock();
        let run = self
            .load_run_unlocked(&request.run_id)?
            .ok_or_else(|| ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown run"))?;
        let principal = run.required_principal()?;
        let work_attempt = run.work_attempt.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "surface lease requires a host-frozen WorkAttempt binding",
            )
        })?;
        work_attempt.validate()?;
        if work_attempt.work_id != request.work_id
            || work_attempt.work_attempt_id != request.work_attempt_id
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "surface lease request does not match the Run WorkAttempt binding",
            ));
        }
        let agent_id = principal.agent_id().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "surface leases require a host-issued durable Agent principal",
            )
        })?;
        let agent_spec_revision = principal.agent_spec_revision().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "surface leases require an exact Agent spec revision",
            )
        })?;
        if work_attempt.agent_id != agent_id
            || work_attempt.agent_spec_revision != agent_spec_revision
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "surface lease WorkAttempt does not match the Run Agent principal",
            ));
        }
        if run.state != ComputerRunState::Ready {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "surface lease requires a ready Computer Run",
            ));
        }
        let (input_domain_id, conflict_domain_id) = {
            let registry = self.inner.surfaces.lock();
            let live = registry
                .by_surface_id
                .get(run.surface.surface_id())
                .ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "run surface is not owned by the live host registry",
                    )
                })?;
            if live.binding != run.surface {
                return Err(ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "run surface incarnation is no longer live",
                ));
            }
            (
                live.input_domain_id.clone(),
                live.conflict_domain_id.clone(),
            )
        };
        // Surface leases are a bounded coordination ledger, not an
        // append-only archive. Retire ordinary terminal records before
        // admission while preserving every active or uncertain dispatch.
        // Mutation receipts remain the independent exact-request replay
        // fence after an acknowledged/known-not-injected lease is retired.
        let leases = self.prune_surface_leases_unlocked(now, 1)?;
        if has_unresolved_uncertainty(&leases, &conflict_domain_id) {
            return Err(uncertain_conflict_domain_error());
        }
        if leases.len() >= MAX_SURFACE_LEASES {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "computer-use surface lease limit reached by active or uncertain dispatches",
            ));
        }
        if leases.iter().any(|lease| {
            lease.work_attempt_id == request.work_attempt_id && !lease.state.is_terminal()
        }) {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "WorkAttempt already owns an active Computer Use surface lease",
            ));
        }
        let queue_sequence = leases
            .iter()
            .map(|lease| lease.queue_sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::LimitReached,
                    "computer-use surface lease queue sequence exhausted",
                )
            })?;
        let lease = ComputerSurfaceLease {
            schema_version: COMPUTER_SURFACE_LEASE_SCHEMA_VERSION,
            lease_id: Uuid::new_v4().to_string(),
            work_id: request.work_id,
            work_attempt_id: request.work_attempt_id,
            agent_id: agent_id.to_string(),
            agent_spec_revision,
            run_id: run.run_id,
            surface: run.surface,
            authority_epoch: run.authority_epoch,
            control_epoch: run.control_epoch,
            frame_epoch: None,
            input_domain_id,
            conflict_domain_id,
            revision: 1,
            expires_at: request.expires_at,
            queue_sequence,
            priority: request.priority,
            state: ComputerSurfaceLeaseState::Queued,
            dispatch: None,
            disposition: None,
            created_at: now,
            updated_at: now,
        };
        lease.validate()?;
        write_json_exclusive(&self.surface_lease_path(&lease.lease_id)?, &lease)
            .map_err(internal_error)?;
        Ok(lease)
    }

    /// Grant the deterministic next waiter for one exact host surface. A
    /// conflict domain owns at most one Granted/Dispatching lease.
    pub(crate) fn grant_next_surface_lease(
        &self,
        surface: &ComputerSurfaceBinding,
        now: DateTime<Utc>,
    ) -> ComputerResult<Option<ComputerSurfaceLease>> {
        surface.validate()?;
        let _guard = self.inner.lock.lock();
        let conflict_domain_id = {
            let registry = self.inner.surfaces.lock();
            let live = registry
                .by_surface_id
                .get(surface.surface_id())
                .ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "surface is not owned by the live host registry",
                    )
                })?;
            if live.binding != *surface {
                return Err(ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "surface incarnation is no longer live",
                ));
            }
            live.conflict_domain_id.clone()
        };
        let mut leases = self.list_surface_leases_unlocked()?;
        self.expire_surface_leases_unlocked(&mut leases, now)?;
        if has_unresolved_uncertainty(&leases, &conflict_domain_id) {
            return Err(uncertain_conflict_domain_error());
        }
        if leases.iter().any(|lease| {
            lease.conflict_domain_id == conflict_domain_id && lease.state.owns_domain_capacity()
        }) {
            return Ok(None);
        }
        let newest_sequence = leases
            .iter()
            .map(|lease| lease.queue_sequence)
            .max()
            .unwrap_or(0);
        let mut candidates = leases
            .into_iter()
            .filter(|lease| {
                lease.conflict_domain_id == conflict_domain_id
                    && lease.state == ComputerSurfaceLeaseState::Queued
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .effective_priority(newest_sequence)
                .cmp(&left.effective_priority(newest_sequence))
                .then_with(|| left.queue_sequence.cmp(&right.queue_sequence))
                .then_with(|| left.lease_id.cmp(&right.lease_id))
        });
        for mut candidate in candidates {
            let run = self.load_run_unlocked(&candidate.run_id)?;
            let valid = run
                .as_ref()
                .is_some_and(|run| candidate.assert_run_fence(run).is_ok());
            if !valid {
                candidate.transition(
                    ComputerSurfaceLeaseState::Quarantined,
                    now,
                    Some("run_fence_changed_before_grant"),
                )?;
                self.write_surface_lease_unlocked(&candidate)?;
                continue;
            }
            candidate.transition(ComputerSurfaceLeaseState::Granted, now, None)?;
            self.write_surface_lease_unlocked(&candidate)?;
            return Ok(Some(candidate));
        }
        Ok(None)
    }

    /// Atomically claims one stable physical dispatch id and freezes the full
    /// lease/run fence before any input can be injected.
    pub(crate) fn prepare_surface_dispatch(
        &self,
        lease_id: &str,
        expected_revision: u64,
        dispatch_id: &str,
        payload_sha256: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerDispatchClaim> {
        validate_id("lease_id", lease_id)?;
        validate_id("dispatch_id", dispatch_id)?;
        validate_payload_digest(payload_sha256)?;
        let _guard = self.inner.lock.lock();
        let leases = self.list_surface_leases_unlocked()?;
        if let Some(existing) = leases.iter().find(|lease| {
            lease
                .dispatch
                .as_ref()
                .is_some_and(|d| d.dispatch_id == dispatch_id)
        }) {
            let dispatch = existing.dispatch.as_ref().expect("matched dispatch exists");
            if existing.lease_id != lease_id || dispatch.payload_sha256 != payload_sha256 {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "dispatch id was reused for another lease or payload",
                ));
            }
            return Ok(match dispatch.state {
                ComputerDispatchState::Acknowledged
                | ComputerDispatchState::KnownNotInjected
                | ComputerDispatchState::Failed => ComputerDispatchClaim::Replay(existing.clone()),
                ComputerDispatchState::Prepared => ComputerDispatchClaim::Pending,
                ComputerDispatchState::Injected | ComputerDispatchState::Uncertain => {
                    ComputerDispatchClaim::Uncertain
                }
            });
        }
        let mut lease = self.load_surface_lease_unlocked(lease_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown lease")
        })?;
        if lease.revision != expected_revision {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface lease revision changed",
            ));
        }
        if lease.state != ComputerSurfaceLeaseState::Granted {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "surface lease is not granted",
            ));
        }
        if lease.frame_epoch.is_none() {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "surface dispatch requires a lease-bound host observation",
            ));
        }
        if lease.expires_at <= now {
            lease.transition(
                ComputerSurfaceLeaseState::Revoked,
                now,
                Some("lease_expired_before_dispatch"),
            )?;
            self.write_surface_lease_unlocked(&lease)?;
            return Err(ComputerError::new(
                ComputerErrorCode::PermissionRevoked,
                "surface lease expired before dispatch",
            ));
        }
        let run = self
            .load_run_unlocked(&lease.run_id)?
            .ok_or_else(|| ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown run"))?;
        lease.assert_run_fence(&run)?;
        self.assert_live_lease_surface(&lease)?;
        if has_unresolved_uncertainty(&leases, &lease.conflict_domain_id) {
            return Err(uncertain_conflict_domain_error());
        }
        if leases.iter().any(|other| {
            other.lease_id != lease.lease_id
                && other.conflict_domain_id == lease.conflict_domain_id
                && other.state == ComputerSurfaceLeaseState::Dispatching
        }) {
            return Err(ComputerError::new(
                ComputerErrorCode::Pending,
                "another lease is dispatching on this conflict domain",
            ));
        }
        lease.transition(ComputerSurfaceLeaseState::Dispatching, now, None)?;
        lease.dispatch = Some(ComputerDispatchRecord {
            schema_version: COMPUTER_DISPATCH_SCHEMA_VERSION,
            dispatch_id: dispatch_id.to_string(),
            payload_sha256: payload_sha256.to_string(),
            state: ComputerDispatchState::Prepared,
            prepared_at: now,
            injected_at: None,
            completed_at: None,
            outcome_sha256: None,
            error_code: None,
        });
        self.write_surface_lease_unlocked(&lease)?;
        Ok(ComputerDispatchClaim::Perform(lease))
    }

    /// Commit the irreversible send boundary before calling the backend. A
    /// restart after this write is Uncertain and is never replayed.
    pub(crate) fn mark_surface_dispatch_injected(
        &self,
        lease_id: &str,
        expected_revision: u64,
        dispatch_id: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerSurfaceLease> {
        let _guard = self.inner.lock.lock();
        let mut lease = self.load_surface_lease_unlocked(lease_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown lease")
        })?;
        if lease.revision != expected_revision
            || lease.state != ComputerSurfaceLeaseState::Dispatching
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface dispatch lease changed before injection",
            ));
        }
        let run = self
            .load_run_unlocked(&lease.run_id)?
            .ok_or_else(|| ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown run"))?;
        lease.assert_run_fence(&run)?;
        self.assert_live_lease_surface(&lease)?;
        let dispatch = lease.dispatch.as_mut().ok_or_else(invalid_lease_record)?;
        if dispatch.dispatch_id != dispatch_id || dispatch.state != ComputerDispatchState::Prepared
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface dispatch id or state changed before injection",
            ));
        }
        dispatch.state = ComputerDispatchState::Injected;
        dispatch.injected_at = Some(now);
        lease.revision = lease.revision.saturating_add(1);
        lease.updated_at = now;
        self.write_surface_lease_unlocked(&lease)?;
        Ok(lease)
    }

    pub(crate) fn acknowledge_surface_dispatch(
        &self,
        lease_id: &str,
        expected_revision: u64,
        dispatch_id: &str,
        outcome_sha256: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerSurfaceLease> {
        validate_payload_digest(outcome_sha256)?;
        let _guard = self.inner.lock.lock();
        let mut lease = self.load_surface_lease_unlocked(lease_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown lease")
        })?;
        if lease.revision != expected_revision
            || lease.state != ComputerSurfaceLeaseState::Dispatching
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface dispatch lease changed before acknowledgement",
            ));
        }
        let dispatch = lease.dispatch.as_mut().ok_or_else(invalid_lease_record)?;
        if dispatch.dispatch_id != dispatch_id || dispatch.state != ComputerDispatchState::Injected
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface dispatch id or state changed before acknowledgement",
            ));
        }
        dispatch.state = ComputerDispatchState::Acknowledged;
        dispatch.completed_at = Some(now);
        dispatch.outcome_sha256 = Some(outcome_sha256.to_string());
        lease.transition(ComputerSurfaceLeaseState::Released, now, None)?;
        self.write_surface_lease_unlocked(&lease)?;
        Ok(lease)
    }

    /// Fail closed at the physical boundary. Prepared is definitely not sent;
    /// Injected is ambiguous and permanently Uncertain.
    pub(crate) fn fail_surface_dispatch(
        &self,
        lease_id: &str,
        dispatch_id: &str,
        error_code: ComputerErrorCode,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerSurfaceLease> {
        let _guard = self.inner.lock.lock();
        let mut lease = self.load_surface_lease_unlocked(lease_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown lease")
        })?;
        let dispatch = lease.dispatch.as_mut().ok_or_else(invalid_lease_record)?;
        if dispatch.dispatch_id != dispatch_id {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface dispatch id changed",
            ));
        }
        let (dispatch_state, lease_state, disposition) = match dispatch.state {
            ComputerDispatchState::Prepared => (
                ComputerDispatchState::KnownNotInjected,
                ComputerSurfaceLeaseState::Revoked,
                "dispatch_failed_before_injection",
            ),
            ComputerDispatchState::Injected => (
                ComputerDispatchState::Uncertain,
                ComputerSurfaceLeaseState::Uncertain,
                "dispatch_outcome_uncertain_after_injection",
            ),
            _ => {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidState,
                    "surface dispatch is already terminal",
                ))
            }
        };
        dispatch.state = dispatch_state;
        dispatch.completed_at = Some(now);
        dispatch.error_code = Some(error_code);
        lease.transition(lease_state, now, Some(disposition))?;
        self.write_surface_lease_unlocked(&lease)?;
        Ok(lease)
    }

    pub(crate) fn list_surface_leases(&self) -> ComputerResult<Vec<ComputerSurfaceLease>> {
        let _guard = self.inner.lock.lock();
        self.list_surface_leases_unlocked()
    }

    /// Acquire the conflict-domain lease before an Agent is allowed to
    /// observe. This prevents multiple Agents from racing on mutually stale
    /// frames of one physical foreground surface.
    pub(crate) fn acquire_agent_surface_observation(
        &self,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<Option<ComputerSurfaceLease>> {
        validate_id("run_id", run_id)?;
        let run = self
            .load_run(run_id)?
            .ok_or_else(|| ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown run"))?;
        let binding = run.work_attempt.clone().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "Agent Computer Run is missing its host-frozen WorkAttempt binding",
            )
        })?;
        let mut active = self
            .list_surface_leases()?
            .into_iter()
            .filter(|lease| lease.run_id == run_id && !lease.state.is_terminal())
            .collect::<Vec<_>>();
        if active.len() > 1 {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "Agent Computer Run owns multiple active surface leases",
            ));
        }
        let lease = if let Some(lease) = active.pop() {
            lease
        } else {
            let run_deadline = run.started_at.unwrap_or(run.created_at)
                + Duration::seconds(run.limits.max_duration_secs as i64);
            if run_deadline <= now {
                return Err(ComputerError::new(
                    ComputerErrorCode::LimitReached,
                    "computer run expired before a surface lease could be issued",
                ));
            }
            self.queue_surface_lease(
                HostSurfaceLeaseRequest {
                    work_id: binding.work_id,
                    work_attempt_id: binding.work_attempt_id,
                    run_id: run_id.to_string(),
                    priority: super::coordination::HostLeasePriority::Normal,
                    expires_at: (now + Duration::minutes(1)).min(run_deadline),
                },
                now,
            )?
        };
        match lease.state {
            ComputerSurfaceLeaseState::Queued => {
                let Some(granted) = self.grant_next_surface_lease(&run.surface, now)? else {
                    return Ok(None);
                };
                if granted.lease_id == lease.lease_id {
                    Ok(Some(granted))
                } else {
                    Ok(None)
                }
            }
            ComputerSurfaceLeaseState::Granted => Ok(Some(lease)),
            ComputerSurfaceLeaseState::Dispatching => Err(ComputerError::new(
                ComputerErrorCode::InvalidState,
                "Agent surface lease is already dispatching",
            )),
            _ => Err(invalid_lease_record()),
        }
    }

    /// Freeze the exact host-stamped observation into the granted lease.
    pub(crate) fn bind_surface_lease_observation(
        &self,
        lease_id: &str,
        expected_revision: u64,
        run_id: &str,
        frame_epoch: u64,
        freshness_tick: u64,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerSurfaceLease> {
        let _guard = self.inner.lock.lock();
        let mut lease = self.load_surface_lease_unlocked(lease_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown lease")
        })?;
        if lease.revision != expected_revision
            || lease.run_id != run_id
            || lease.state != ComputerSurfaceLeaseState::Granted
            || frame_epoch == 0
            || freshness_tick == 0
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface lease changed before observation binding",
            ));
        }
        let run = self
            .load_run_unlocked(run_id)?
            .ok_or_else(|| ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown run"))?;
        let observation = run.current_observation.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "Computer Run has no committed observation to bind",
            )
        })?;
        if observation.authority.frame_epoch != frame_epoch
            || observation.authority.freshness.tick != freshness_tick
        {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "Computer Run observation does not match the lease frame",
            ));
        }
        lease.frame_epoch = Some(frame_epoch);
        lease.revision = lease.revision.saturating_add(1);
        lease.updated_at = now;
        lease.assert_run_fence(&run)?;
        self.assert_live_lease_surface(&lease)?;
        self.write_surface_lease_unlocked(&lease)?;
        Ok(lease)
    }

    #[cfg(test)]
    pub(crate) fn load_surface_lease(
        &self,
        lease_id: &str,
    ) -> ComputerResult<Option<ComputerSurfaceLease>> {
        let _guard = self.inner.lock.lock();
        self.load_surface_lease_unlocked(lease_id)
    }

    /// Acquire and prepare the one physical dispatch for an Agent Run. This is
    /// the production coordination seam used by `ComputerUseService::act`;
    /// local-operator Runs continue through the existing singleton path.
    pub(crate) fn acquire_agent_surface_dispatch(
        &self,
        run_id: &str,
        request_id: &str,
        payload_sha256: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerDispatchClaim> {
        validate_id("run_id", run_id)?;
        validate_id("request_id", request_id)?;
        validate_payload_digest(payload_sha256)?;
        let run = self
            .load_run(run_id)?
            .ok_or_else(|| ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown run"))?;
        run.work_attempt.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "Agent Computer Run is missing its host-frozen WorkAttempt binding",
            )
        })?;
        let mut active = self
            .list_surface_leases()?
            .into_iter()
            .filter(|lease| lease.run_id == run_id && !lease.state.is_terminal())
            .collect::<Vec<_>>();
        if active.len() > 1 {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "Agent Computer Run owns multiple active surface leases",
            ));
        }
        let lease = active.pop().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "Agent action requires a previously granted observation lease",
            )
        })?;
        if lease.state == ComputerSurfaceLeaseState::Queued {
            return Ok(ComputerDispatchClaim::Pending);
        }
        if lease.state == ComputerSurfaceLeaseState::Dispatching {
            return Ok(
                match lease.dispatch.as_ref().map(|dispatch| dispatch.state) {
                    Some(ComputerDispatchState::Prepared) => ComputerDispatchClaim::Pending,
                    Some(ComputerDispatchState::Injected | ComputerDispatchState::Uncertain) => {
                        ComputerDispatchClaim::Uncertain
                    }
                    Some(_) => ComputerDispatchClaim::Replay(lease),
                    None => return Err(invalid_lease_record()),
                },
            );
        }
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"grokptah-computer-dispatch-v1\0");
        hasher.update(run_id.as_bytes());
        hasher.update([0]);
        hasher.update(request_id.as_bytes());
        let dispatch_id = format!("{:x}", hasher.finalize());
        self.prepare_surface_dispatch(
            &lease.lease_id,
            lease.revision,
            &dispatch_id,
            payload_sha256,
            now,
        )
    }

    pub(crate) fn revoke_surface_lease_before_dispatch(
        &self,
        lease_id: &str,
        expected_revision: u64,
        disposition: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerSurfaceLease> {
        validate_id("lease_id", lease_id)?;
        validate_id("disposition", disposition)?;
        let _guard = self.inner.lock.lock();
        let mut lease = self.load_surface_lease_unlocked(lease_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown lease")
        })?;
        if lease.revision != expected_revision
            || !matches!(
                lease.state,
                ComputerSurfaceLeaseState::Queued | ComputerSurfaceLeaseState::Granted
            )
            || lease.dispatch.is_some()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "surface lease changed before revocation",
            ));
        }
        lease.transition(ComputerSurfaceLeaseState::Revoked, now, Some(disposition))?;
        self.write_surface_lease_unlocked(&lease)?;
        Ok(lease)
    }

    pub(crate) fn replay_mutation(
        &self,
        request_id: &str,
        operation: &str,
        payload_hash: &str,
        stamp: &MutationStamp,
    ) -> ComputerResult<Option<ComputerResult<serde_json::Value>>> {
        let _guard = self.inner.lock.lock();
        let path = self.receipt_path(request_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(self.interpret_existing_receipt(
            &self.read_receipt_path(&path)?,
            request_id,
            operation,
            payload_hash,
            stamp,
        )?))
    }

    pub(crate) fn claim_mutation(
        &self,
        request_id: &str,
        operation: &str,
        payload_hash: &str,
        stamp: &MutationStamp,
    ) -> ComputerResult<MutationClaim> {
        let _guard = self.inner.lock.lock();
        let path = self.receipt_path(request_id)?;
        if path.is_file() {
            return Ok(
                match self.interpret_existing_receipt(
                    &self.read_receipt_path(&path)?,
                    request_id,
                    operation,
                    payload_hash,
                    stamp,
                )? {
                    Ok(value) => MutationClaim::Replay(Ok(value)),
                    Err(error) if error.code == ComputerErrorCode::Pending => {
                        MutationClaim::Pending
                    }
                    Err(error) if error.code == ComputerErrorCode::UncertainOutcome => {
                        MutationClaim::Uncertain
                    }
                    Err(error) => MutationClaim::Replay(Err(error)),
                },
            );
        }
        if count_json_files(&self.inner.root.join("receipts")).map_err(internal_error)?
            >= MAX_RECEIPTS
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "computer-use mutation receipt limit reached",
            ));
        }
        let now = Utc::now();
        let receipt = MutationReceipt {
            request_id: request_id.into(),
            operation: operation.into(),
            payload_hash: payload_hash.into(),
            state: ReceiptState::Claimed,
            created_at: now,
            updated_at: now,
            result: None,
            error: None,
            schema_version: COMPUTER_RECEIPT_SCHEMA_VERSION,
            caller_kind: Some(stamp.kind().to_string()),
            caller_owner_session_id: stamp.owner_session_id(),
            caller_agent_id: stamp.agent_id(),
            caller_agent_spec_revision: stamp.agent_spec_revision(),
            run_id: stamp.run_id.clone(),
            surface_id: stamp.surface_id.clone(),
            incarnation: stamp.incarnation.clone(),
            grant_id: stamp.grant_id.clone(),
            frame_epoch: stamp.frame_epoch,
            pre_authority_epoch: Some(stamp.pre_authority_epoch),
            pre_control_epoch: Some(stamp.pre_control_epoch),
            post_authority_epoch: None,
            post_control_epoch: None,
        };
        write_json_exclusive(&path, &receipt).map_err(internal_error)?;
        Ok(MutationClaim::Perform)
    }

    fn interpret_existing_receipt(
        &self,
        receipt: &MutationReceipt,
        request_id: &str,
        operation: &str,
        payload_hash: &str,
        stamp: &MutationStamp,
    ) -> ComputerResult<ComputerResult<serde_json::Value>> {
        if !receipt_is_stamped(receipt) {
            return Ok(Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "the earlier computer-use mutation has an uncertain outcome and will not be retried",
            )));
        }
        if receipt.request_id != request_id
            || receipt.operation != operation
            || receipt.payload_hash != payload_hash
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "request id was reused with a different computer-use mutation",
            ));
        }
        if !receipt_matches_stamp(receipt, stamp) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "idempotency receipt is not bound to this caller and run authority",
            ));
        }
        Ok(match receipt.state {
            ReceiptState::Claimed => Err(ComputerError::new(
                ComputerErrorCode::Pending,
                "an identical computer-use mutation is in progress",
            )),
            ReceiptState::Uncertain => Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "the earlier computer-use mutation has an uncertain outcome and will not be retried",
            )),
            ReceiptState::Succeeded => Ok(receipt.result.clone().unwrap_or(serde_json::Value::Null)),
            ReceiptState::Failed => Err(receipt.error.clone().unwrap_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Internal,
                    "stored mutation failed without an error",
                )
            })),
        })
    }

    pub(crate) fn complete_mutation(
        &self,
        request_id: &str,
        result: &ComputerResult<serde_json::Value>,
    ) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        let path = self.receipt_path(request_id)?;
        let mut receipt = self.read_receipt_path(&path)?;
        if receipt.state != ReceiptState::Claimed {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "computer-use mutation receipt is already terminal",
            ));
        }
        receipt.updated_at = Utc::now();
        match result {
            Ok(value) => {
                receipt.state = ReceiptState::Succeeded;
                receipt.result = Some(value.clone());
                if let Ok(run) = serde_json::from_value::<ComputerRun>(value.clone()) {
                    receipt.run_id.get_or_insert(run.run_id.clone());
                    if run.surface.is_issued() {
                        receipt
                            .surface_id
                            .get_or_insert(run.surface.surface_id.clone());
                        receipt
                            .incarnation
                            .get_or_insert(run.surface.incarnation.clone());
                    }
                    receipt.post_authority_epoch = Some(run.authority_epoch);
                    receipt.post_control_epoch = Some(run.control_epoch);
                } else if let Some(run_id) = receipt.run_id.clone() {
                    if let Ok(Some(run)) = self.load_run_unlocked(&run_id) {
                        receipt.post_authority_epoch = Some(run.authority_epoch);
                        receipt.post_control_epoch = Some(run.control_epoch);
                    }
                }
            }
            Err(error) => {
                receipt.state = ReceiptState::Failed;
                receipt.error = Some(error.clone());
                if let Some(run_id) = receipt.run_id.clone() {
                    if let Ok(Some(run)) = self.load_run_unlocked(&run_id) {
                        receipt.post_authority_epoch = Some(run.authority_epoch);
                        receipt.post_control_epoch = Some(run.control_epoch);
                    }
                }
            }
        }
        atomic_write_json(&path, &receipt).map_err(internal_error)
    }

    fn run_path(&self, run_id: &str) -> ComputerResult<PathBuf> {
        let safe = safe_file_id(run_id)?;
        Ok(self.inner.root.join("runs").join(format!("{safe}.json")))
    }

    fn surface_lease_path(&self, lease_id: &str) -> ComputerResult<PathBuf> {
        let safe = safe_file_id(lease_id)?;
        Ok(self
            .inner
            .root
            .join("surface-leases")
            .join(format!("{safe}.json")))
    }

    pub(crate) fn receipt_path(&self, request_id: &str) -> ComputerResult<PathBuf> {
        let safe = safe_file_id(request_id)?;
        Ok(self
            .inner
            .root
            .join("receipts")
            .join(format!("{safe}.json")))
    }

    fn load_run_unlocked(&self, run_id: &str) -> ComputerResult<Option<ComputerRun>> {
        let path = self.run_path(run_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        self.read_run_path(&path).map(Some)
    }

    fn list_runs_unlocked(&self) -> ComputerResult<Vec<ComputerRun>> {
        let mut runs = Vec::new();
        for path in json_paths(&self.inner.root.join("runs")).map_err(internal_error)? {
            runs.push(self.read_run_path(&path)?);
        }
        runs.sort_by(|a: &ComputerRun, b: &ComputerRun| b.created_at.cmp(&a.created_at));
        Ok(runs)
    }

    fn load_surface_lease_unlocked(
        &self,
        lease_id: &str,
    ) -> ComputerResult<Option<ComputerSurfaceLease>> {
        let path = self.surface_lease_path(lease_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        self.read_surface_lease_path(&path).map(Some)
    }

    fn list_surface_leases_unlocked(&self) -> ComputerResult<Vec<ComputerSurfaceLease>> {
        let mut leases = Vec::new();
        for path in json_paths(&self.inner.root.join("surface-leases")).map_err(internal_error)? {
            leases.push(self.read_surface_lease_path(&path)?);
        }
        leases.sort_by(|left, right| {
            left.queue_sequence
                .cmp(&right.queue_sequence)
                .then_with(|| left.lease_id.cmp(&right.lease_id))
        });
        Ok(leases)
    }

    /// Keep the durable lease ledger bounded without weakening a physical
    /// dispatch fence. Active and uncertain records are never removed. Older
    /// ordinary terminal records are retired by age first, then oldest-first
    /// only as needed to reserve admission capacity.
    fn prune_surface_leases_unlocked(
        &self,
        now: DateTime<Utc>,
        reserve_slots: usize,
    ) -> ComputerResult<Vec<ComputerSurfaceLease>> {
        if reserve_slots > MAX_SURFACE_LEASES {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "surface lease retention requested impossible capacity",
            ));
        }
        let mut leases = self.list_surface_leases_unlocked()?;
        let mut candidates: Vec<&ComputerSurfaceLease> = leases
            .iter()
            .filter(|lease| lease.state.is_retention_prunable())
            .collect();
        candidates.sort_by(|left, right| {
            left.updated_at
                .cmp(&right.updated_at)
                .then_with(|| left.queue_sequence.cmp(&right.queue_sequence))
                .then_with(|| left.lease_id.cmp(&right.lease_id))
        });

        let mut remove = HashSet::new();
        for lease in &candidates {
            if now.signed_duration_since(lease.updated_at) > TERMINAL_SURFACE_LEASE_AGE {
                remove.insert(lease.lease_id.clone());
            }
        }
        let retained_after_age = leases.len().saturating_sub(remove.len());
        let capacity_prune = retained_after_age
            .saturating_add(reserve_slots)
            .saturating_sub(MAX_SURFACE_LEASES);
        let capacity_retirements = candidates
            .into_iter()
            .filter(|lease| !remove.contains(&lease.lease_id))
            .take(capacity_prune)
            .map(|lease| lease.lease_id.clone())
            .collect::<Vec<_>>();
        remove.extend(capacity_retirements);

        if !remove.is_empty() {
            for lease_id in &remove {
                fs::remove_file(self.surface_lease_path(lease_id)?).map_err(internal_error)?;
            }
            leases.retain(|lease| !remove.contains(&lease.lease_id));
        }
        Ok(leases)
    }

    fn write_surface_lease_unlocked(&self, lease: &ComputerSurfaceLease) -> ComputerResult<()> {
        lease.validate()?;
        atomic_write_json(&self.surface_lease_path(&lease.lease_id)?, lease).map_err(internal_error)
    }

    fn read_surface_lease_path(&self, path: &Path) -> ComputerResult<ComputerSurfaceLease> {
        let lease: ComputerSurfaceLease = read_json(path).map_err(internal_error)?;
        lease.validate()?;
        if self.surface_lease_path(&lease.lease_id)? != path {
            return Err(invalid_lease_record());
        }
        Ok(lease)
    }

    fn assert_live_lease_surface(&self, lease: &ComputerSurfaceLease) -> ComputerResult<()> {
        let registry = self.inner.surfaces.lock();
        let live = registry
            .by_surface_id
            .get(lease.surface.surface_id())
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "lease surface is not owned by the live host registry",
                )
            })?;
        if live.binding != lease.surface
            || live.input_domain_id != lease.input_domain_id
            || live.conflict_domain_id != lease.conflict_domain_id
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "lease surface or conflict domain is stale or forged",
            ));
        }
        if let Some(frame_epoch) = lease.frame_epoch {
            if live.frame_epoch.load(Ordering::SeqCst) != frame_epoch {
                return Err(ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "lease frame is no longer the exact current host surface frame",
                ));
            }
        }
        Ok(())
    }

    fn expire_surface_leases_unlocked(
        &self,
        leases: &mut [ComputerSurfaceLease],
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        for lease in leases {
            if !lease.state.is_terminal() && lease.expires_at <= now {
                if lease.state == ComputerSurfaceLeaseState::Dispatching {
                    let dispatch = lease.dispatch.as_mut().ok_or_else(invalid_lease_record)?;
                    match dispatch.state {
                        ComputerDispatchState::Prepared => {
                            dispatch.state = ComputerDispatchState::KnownNotInjected;
                            dispatch.completed_at = Some(now);
                            dispatch.error_code = Some(ComputerErrorCode::PermissionRevoked);
                            lease.transition(
                                ComputerSurfaceLeaseState::Revoked,
                                now,
                                Some("lease_expired_before_injection"),
                            )?;
                        }
                        ComputerDispatchState::Injected => {
                            dispatch.state = ComputerDispatchState::Uncertain;
                            dispatch.completed_at = Some(now);
                            dispatch.error_code = Some(ComputerErrorCode::UncertainOutcome);
                            lease.transition(
                                ComputerSurfaceLeaseState::Uncertain,
                                now,
                                Some("lease_expired_after_injection"),
                            )?;
                        }
                        _ => return Err(invalid_lease_record()),
                    }
                } else {
                    lease.transition(
                        ComputerSurfaceLeaseState::Revoked,
                        now,
                        Some("lease_expired"),
                    )?;
                }
                self.write_surface_lease_unlocked(lease)?;
            }
        }
        Ok(())
    }

    fn recover_interrupted(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        for path in json_paths(&self.inner.root.join("runs")).map_err(internal_error)? {
            let mut run = self.read_run_path(&path)?;
            if run.state.is_terminal() {
                continue;
            }
            run.state = ComputerRunState::Interrupted;
            run.set_control_disposition(ComputerControlDisposition::Interrupted);
            run.version = run.version.saturating_add(1);
            run.updated_at = Utc::now();
            run.ended_at = Some(run.updated_at);
            run.grant = None;
            run.current_observation = None;
            if run.surface.is_issued() {
                if let Ok(rotated) = run.surface.rotate_incarnation() {
                    run.surface = rotated;
                }
            }
            run.freshness_tick = 0;
            run.bump_authority_epoch();
            run.capability_proof = match &run.capability_proof {
                crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain { .. }
                | crate::computer_use::ComputerCapabilityProof::MeasuredBackgroundSafeSemantic { .. } => {
                    crate::computer_use::ComputerCapabilityProof::Unproven
                }
                other => other.clone(),
            };
            // A prior action summary can carry backend-chosen text. Clearing it
            // is the same fail-closed move as dropping the observation: restart
            // must not keep a leaky last_outcome on the durable record.
            run.last_outcome = None;
            run.last_error = Some(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "computer run interrupted by process restart; explicit reauthorization required",
            ));
            // The interruption must be visible in the durable journal itself,
            // not only on the run record, so a coordinator replaying events
            // sees why the run ended (#286).
            run.record_audit(
                "recover",
                "interrupted",
                None,
                None,
                Some(ComputerErrorCode::Interrupted),
            );
            validate_run_record(&run)?;
            atomic_write_json(&path, &run).map_err(internal_error)?;
        }
        Ok(())
    }

    fn recover_surface_leases(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        let now = Utc::now();
        for path in json_paths(&self.inner.root.join("surface-leases")).map_err(internal_error)? {
            let mut lease = self.read_surface_lease_path(&path)?;
            if lease.state.is_terminal() {
                continue;
            }
            match lease.state {
                ComputerSurfaceLeaseState::Queued | ComputerSurfaceLeaseState::Granted => {
                    lease.transition(
                        ComputerSurfaceLeaseState::Revoked,
                        now,
                        Some("restart_invalidated_surface_incarnation"),
                    )?;
                }
                ComputerSurfaceLeaseState::Dispatching => {
                    let dispatch = lease.dispatch.as_mut().ok_or_else(invalid_lease_record)?;
                    match dispatch.state {
                        ComputerDispatchState::Prepared => {
                            dispatch.state = ComputerDispatchState::KnownNotInjected;
                            dispatch.completed_at = Some(now);
                            dispatch.error_code = Some(ComputerErrorCode::Interrupted);
                            lease.transition(
                                ComputerSurfaceLeaseState::Revoked,
                                now,
                                Some("restart_before_physical_injection"),
                            )?;
                        }
                        ComputerDispatchState::Injected => {
                            dispatch.state = ComputerDispatchState::Uncertain;
                            dispatch.completed_at = Some(now);
                            dispatch.error_code = Some(ComputerErrorCode::UncertainOutcome);
                            lease.transition(
                                ComputerSurfaceLeaseState::Uncertain,
                                now,
                                Some("restart_after_physical_injection"),
                            )?;
                        }
                        _ => return Err(invalid_lease_record()),
                    }
                }
                _ => return Err(invalid_lease_record()),
            }
            self.write_surface_lease_unlocked(&lease)?;
        }
        Ok(())
    }

    fn recover_receipts(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        for path in json_paths(&self.inner.root.join("receipts")).map_err(internal_error)? {
            let mut receipt = self.read_receipt_path(&path)?;
            if receipt.state != ReceiptState::Claimed {
                continue;
            }
            receipt.state = ReceiptState::Uncertain;
            receipt.updated_at = Utc::now();
            receipt.error = Some(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "process stopped while the computer-use mutation was in flight; it will not be retried automatically",
            ));
            atomic_write_json(&path, &receipt).map_err(internal_error)?;
        }
        Ok(())
    }

    fn prune_retention(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        let now = Utc::now();

        let mut runs = Vec::new();
        for path in json_paths(&self.inner.root.join("runs")).map_err(internal_error)? {
            let run = self.read_run_path(&path)?;
            runs.push((path, run));
        }
        runs.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
        for (index, (path, run)) in runs.into_iter().enumerate() {
            let expired = run.state.is_terminal()
                && now.signed_duration_since(run.updated_at) > TERMINAL_RUN_AGE;
            if run.state.is_terminal() && (index >= Self::MAX_RUN_RECORDS || expired) {
                fs::remove_file(path).map_err(internal_error)?;
            }
        }

        let mut receipts = Vec::new();
        for path in json_paths(&self.inner.root.join("receipts")).map_err(internal_error)? {
            let receipt = self.read_receipt_path(&path)?;
            receipts.push((path, receipt));
        }
        receipts.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
        for (index, (path, receipt)) in receipts.into_iter().enumerate() {
            let terminal = receipt.state != ReceiptState::Claimed;
            let expired =
                terminal && now.signed_duration_since(receipt.updated_at) > TERMINAL_RECEIPT_AGE;
            if terminal && (index >= MAX_RECEIPTS || expired) {
                fs::remove_file(path).map_err(internal_error)?;
            }
        }
        self.prune_surface_leases_unlocked(now, 0)?;
        Ok(())
    }

    fn read_run_path(&self, path: &Path) -> ComputerResult<ComputerRun> {
        let mut run: ComputerRun = read_json(path).map_err(internal_error)?;
        if migrate_run_record(&mut run)? {
            atomic_write_json(path, &run).map_err(internal_error)?;
        }
        validate_run_record(&run)?;
        if self.run_path(&run.run_id)? != path {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "computer-use run record identity does not match its durable path",
            ));
        }
        Ok(run)
    }

    fn read_receipt_path(&self, path: &Path) -> ComputerResult<MutationReceipt> {
        let receipt: MutationReceipt = read_json(path).map_err(internal_error)?;
        validate_receipt(&receipt)?;
        if self.receipt_path(&receipt.request_id)? != path {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "computer-use receipt identity does not match its durable path",
            ));
        }
        Ok(receipt)
    }
}

fn migrate_run_record(run: &mut ComputerRun) -> ComputerResult<bool> {
    if run.schema_version != 0 && run.schema_version != COMPUTER_RUN_SCHEMA_VERSION {
        return Err(invalid_record());
    }
    let mut changed = false;
    let mut untrusted = run.schema_version == 0;

    if run
        .initiating_principal
        .as_ref()
        .is_some_and(|principal| principal.validate().is_err())
    {
        run.initiating_principal = None;
        untrusted = true;
        changed = true;
    }
    if run.initiating_principal.as_ref().is_some_and(|principal| {
        principal
            .session_id()
            .is_some_and(|session_id| session_id != run.owner_session_id)
    }) {
        run.initiating_principal = None;
        untrusted = true;
        changed = true;
    }
    if run
        .initiating_principal
        .as_ref()
        .is_some_and(|principal| principal.agent_id().is_some())
        && run.work_attempt.is_none()
    {
        // Pre-coordination Agent-shaped principals were never backed by an
        // exact host-resolved WorkAttempt and cannot be grandfathered into
        // authority merely because their string shape is valid.
        run.initiating_principal = None;
        untrusted = true;
        changed = true;
    }

    let isolated_mismatch = run
        .capability_proof
        .isolated_surface()
        .is_some_and(|surface| !run.surface.is_issued() || surface != run.surface);
    let proof_invalid = run.capability_proof.validate().is_err()
        || matches!(
            run.capability_proof,
            crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
                origin: crate::computer_use::IsolationProofOrigin::HostNative,
                ..
            }
        )
        || isolated_mismatch;
    if proof_invalid
        || matches!(
            run.capability_proof.tier(),
            crate::computer_use::ComputerCapabilityTier::IndependentlyIsolatedVisualInputDomain
                | crate::computer_use::ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
        ) && run.schema_version == 0
    {
        if !matches!(
            run.capability_proof,
            crate::computer_use::ComputerCapabilityProof::Unproven
        ) {
            run.capability_proof = crate::computer_use::ComputerCapabilityProof::Unproven;
            changed = true;
        }
        untrusted = true;
    }

    if let Some(grant) = &run.grant {
        let grant_untrusted = if grant.revoked_at.is_some() {
            grant.run_id != run.run_id
        } else {
            grant.validate().is_err()
                || grant.run_id != run.run_id
                || grant.target != run.target
                || grant.surface != run.surface
                || grant.authority_epoch != run.authority_epoch
                || grant.principal.as_ref() != run.initiating_principal.as_ref()
                || grant.capability_tier != run.capability_proof.tier()
                || matches!(
                    grant.capability_tier,
                    crate::computer_use::ComputerCapabilityTier::Unproven
                )
        };
        if grant_untrusted {
            run.grant = None;
            untrusted = true;
            changed = true;
        }
    }

    if let Some(observation) = &run.current_observation {
        let observation_untrusted = observation.validate(&run.limits).is_err()
            || observation.target != run.target
            || !observation.authority.surface.is_issued()
            || observation.authority.surface != run.surface
            || observation.authority.frame_epoch == 0
            || observation.authority.freshness.tick == 0
            || observation.authority.freshness.surface_id != run.surface.surface_id
            || observation.authority.freshness.incarnation != run.surface.incarnation
            || observation.authority.authority_epoch != run.authority_epoch
            || observation.authority.control_epoch != run.control_epoch
            || observation.authority.target_generation != run.target.generation;
        if observation_untrusted {
            run.current_observation = None;
            untrusted = true;
            changed = true;
        }
    }

    if untrusted {
        if run.grant.is_some() {
            run.grant = None;
            changed = true;
        }
        if run.current_observation.is_some() {
            run.current_observation = None;
            changed = true;
        }
        if run.schema_version != COMPUTER_RUN_SCHEMA_VERSION {
            run.schema_version = COMPUTER_RUN_SCHEMA_VERSION;
            changed = true;
        }
    } else if run.schema_version != COMPUTER_RUN_SCHEMA_VERSION {
        run.schema_version = COMPUTER_RUN_SCHEMA_VERSION;
        changed = true;
    }
    Ok(changed)
}

fn validate_run_record(run: &ComputerRun) -> ComputerResult<()> {
    validate_id("run_id", &run.run_id)?;
    validate_workspace(run.workspace.as_deref())?;
    if let Some(parent_run_id) = &run.parent_run_id {
        validate_id("parent_run_id", parent_run_id)?;
    }
    if let Some(campaign_id) = &run.campaign_id {
        validate_id("campaign_id", campaign_id)?;
    }
    run.target.validate()?;
    run.limits.validate()?;
    if run.schema_version != COMPUTER_RUN_SCHEMA_VERSION {
        return Err(invalid_record());
    }
    if run.surface.is_issued() {
        run.surface.validate()?;
    } else if !matches!(
        run.capability_proof,
        crate::computer_use::ComputerCapabilityProof::Unproven
    ) {
        return Err(invalid_record());
    }
    run.capability_proof.validate()?;
    if matches!(
        run.capability_proof,
        crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
            origin: crate::computer_use::IsolationProofOrigin::HostNative,
            ..
        }
    ) {
        return Err(invalid_record());
    }
    if let Some(isolated) = run.capability_proof.isolated_surface() {
        if isolated != run.surface {
            return Err(invalid_record());
        }
    }
    if let Some(principal) = &run.initiating_principal {
        principal.validate()?;
        if principal
            .session_id()
            .is_some_and(|session_id| session_id != run.owner_session_id)
        {
            return Err(invalid_record());
        }
    }
    if let Some(binding) = &run.work_attempt {
        binding.validate()?;
        let Some(principal) = &run.initiating_principal else {
            return Err(invalid_record());
        };
        if principal.agent_id() != Some(binding.agent_id.as_str())
            || principal.agent_spec_revision() != Some(binding.agent_spec_revision)
        {
            return Err(invalid_record());
        }
    }
    if run.version == 0
        || run.action_count > run.limits.max_actions
        || run.evidence_bytes > run.limits.max_evidence_bytes
        || run.audit.len() > 1_024
        || run
            .last_outcome
            .as_ref()
            .is_some_and(|outcome| outcome.summary.len() > 512)
        || run
            .last_error
            .as_ref()
            .is_some_and(|error| error.message.len() > 512)
    {
        return Err(invalid_record());
    }
    if run.state.is_terminal() != run.ended_at.is_some() {
        return Err(invalid_record());
    }
    if (run.control_disposition == ComputerControlDisposition::OperatorTakeover
        && run.state != ComputerRunState::Paused)
        || (run.control_disposition == ComputerControlDisposition::Interrupted
            && run.state != ComputerRunState::Interrupted)
        || (run.control_disposition == ComputerControlDisposition::Stopped
            && !run.state.is_terminal())
    {
        return Err(invalid_record());
    }
    if let Some(observation) = &run.current_observation {
        observation.validate(&run.limits)?;
        if observation.target != run.target
            || run.state.is_terminal()
            || observation.authority.surface != run.surface
            || observation.authority.frame_epoch == 0
            || observation.authority.freshness.tick == 0
            || observation.authority.freshness.surface_id != run.surface.surface_id
            || observation.authority.freshness.incarnation != run.surface.incarnation
            || observation.authority.authority_epoch != run.authority_epoch
            || observation.authority.control_epoch != run.control_epoch
            || observation.authority.target_generation != run.target.generation
        {
            return Err(invalid_record());
        }
    }
    if let Some(grant) = &run.grant {
        if grant.run_id != run.run_id {
            return Err(invalid_record());
        }
        if grant.revoked_at.is_some() {
            // Revoked grants are retained as bookkeeping. Revocation bumps the
            // run authority epoch, so they must not be required to match live
            // epochs or remaining uses.
        } else {
            grant.validate()?;
            if grant.target != run.target
                || grant.surface != run.surface
                || grant.authority_epoch != run.authority_epoch
                || grant.principal.as_ref() != run.initiating_principal.as_ref()
                || grant.capability_tier != run.capability_proof.tier()
                || grant
                    .uses_remaining
                    .is_some_and(|remaining| remaining > run.limits.max_actions)
            {
                return Err(invalid_record());
            }
        }
    }
    let authority_must_be_revoked =
        run.state.is_terminal() || run.state == ComputerRunState::Paused;
    if authority_must_be_revoked
        && run
            .grant
            .as_ref()
            .is_some_and(|grant| grant.revoked_at.is_none())
    {
        return Err(invalid_record());
    }
    if run.state == ComputerRunState::AwaitingAuthorization
        && (run.grant.is_some() || run.current_observation.is_some())
    {
        return Err(invalid_record());
    }
    if run
        .audit
        .windows(2)
        .any(|entries| entries[0].sequence >= entries[1].sequence)
        || run.audit.iter().any(|entry| {
            entry.operation.len() > 64
                || entry.disposition.len() > 64
                || entry
                    .observation_id
                    .as_ref()
                    .is_some_and(|id| id.len() > super::types::MAX_ID_BYTES)
        })
    {
        return Err(invalid_record());
    }
    Ok(())
}

fn receipt_is_stamped(receipt: &MutationReceipt) -> bool {
    receipt.schema_version == COMPUTER_RECEIPT_SCHEMA_VERSION
        && receipt.caller_kind.is_some()
        && receipt.pre_authority_epoch.is_some()
        && receipt.pre_control_epoch.is_some()
}

fn receipt_matches_stamp(receipt: &MutationReceipt, stamp: &MutationStamp) -> bool {
    receipt.caller_kind.as_deref() == Some(stamp.kind())
        && receipt.caller_owner_session_id == stamp.owner_session_id()
        && receipt.caller_agent_id == stamp.agent_id()
        && receipt.caller_agent_spec_revision == stamp.agent_spec_revision()
        && receipt.run_id == stamp.run_id
        && receipt.surface_id == stamp.surface_id
        && receipt.incarnation == stamp.incarnation
        && receipt.grant_id == stamp.grant_id
}

fn validate_receipt(receipt: &MutationReceipt) -> ComputerResult<()> {
    validate_id("request_id", &receipt.request_id)?;
    if receipt.schema_version != 0 && receipt.schema_version != COMPUTER_RECEIPT_SCHEMA_VERSION {
        return Err(invalid_record());
    }
    if receipt.operation.is_empty()
        || receipt.operation.len() > 64
        || receipt.payload_hash.len() != 64
        || !receipt
            .payload_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_record());
    }
    let payload_shape_is_valid = match receipt.state {
        ReceiptState::Claimed => receipt.result.is_none() && receipt.error.is_none(),
        ReceiptState::Succeeded => receipt.result.is_some() && receipt.error.is_none(),
        ReceiptState::Failed | ReceiptState::Uncertain => {
            receipt.result.is_none() && receipt.error.is_some()
        }
    };
    if !payload_shape_is_valid
        || receipt
            .error
            .as_ref()
            .is_some_and(|error| error.message.len() > 512)
    {
        return Err(invalid_record());
    }
    if receipt.schema_version == COMPUTER_RECEIPT_SCHEMA_VERSION {
        if receipt.caller_kind.is_none()
            || receipt.pre_authority_epoch.is_none()
            || receipt.pre_control_epoch.is_none()
        {
            return Err(invalid_record());
        }
        if receipt.run_id.is_some()
            && (receipt
                .run_id
                .as_deref()
                .is_some_and(|run_id| validate_id("run_id", run_id).is_err())
                || receipt
                    .surface_id
                    .as_deref()
                    .is_some_and(|surface_id| validate_id("surface_id", surface_id).is_err())
                || receipt
                    .incarnation
                    .as_deref()
                    .is_some_and(|incarnation| validate_id("incarnation", incarnation).is_err())
                || receipt
                    .grant_id
                    .as_deref()
                    .is_some_and(|grant_id| validate_id("grant_id", grant_id).is_err()))
        {
            return Err(invalid_record());
        }
    }
    Ok(())
}

fn invalid_record() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::Internal,
        "invalid computer-use durable record",
    )
}

fn safe_file_id(id: &str) -> ComputerResult<String> {
    crate::orchestration::safe_id_filename(id).map_err(|_| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "invalid durable record id",
        )
    })
}

fn json_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn count_json_files(dir: &Path) -> std::io::Result<usize> {
    Ok(json_paths(dir)?.len())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    if fs::metadata(path)?.len() > MAX_RECORD_BYTES {
        anyhow::bail!("computer-use durable record exceeds the size limit");
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().expect("record path has parent"))?.sync_all()?;
    Ok(())
}

fn write_json_exclusive<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    #[cfg(unix)]
    fs::File::open(path.parent().expect("record path has parent"))?.sync_all()?;
    Ok(())
}

fn internal_error(error: impl ToString) -> ComputerError {
    ComputerError::new(ComputerErrorCode::Internal, error.to_string())
}

fn has_unresolved_uncertainty(leases: &[ComputerSurfaceLease], conflict_domain_id: &str) -> bool {
    leases.iter().any(|lease| {
        lease.conflict_domain_id == conflict_domain_id
            && lease.state == ComputerSurfaceLeaseState::Uncertain
    })
}

fn uncertain_conflict_domain_error() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::UncertainOutcome,
        "the physical Computer input domain has an unresolved uncertain dispatch and cannot be reassigned",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Duration;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::computer_use::{
        ActionClass, ActionGrant, ActionOutcome, ComputerTarget, ComputerUseLimits,
    };

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: crate::computer_use::Sensitivity::None,
        }
    }

    fn terminal_surface_lease(
        sequence: u64,
        state: ComputerSurfaceLeaseState,
        now: DateTime<Utc>,
    ) -> ComputerSurfaceLease {
        let dispatch = match state {
            ComputerSurfaceLeaseState::Released => Some(ComputerDispatchRecord {
                schema_version: COMPUTER_DISPATCH_SCHEMA_VERSION,
                dispatch_id: format!("dispatch-{sequence}"),
                payload_sha256: "a".repeat(64),
                state: ComputerDispatchState::Acknowledged,
                prepared_at: now - Duration::seconds(2),
                injected_at: Some(now - Duration::seconds(1)),
                completed_at: Some(now),
                outcome_sha256: Some("b".repeat(64)),
                error_code: None,
            }),
            ComputerSurfaceLeaseState::Uncertain => Some(ComputerDispatchRecord {
                schema_version: COMPUTER_DISPATCH_SCHEMA_VERSION,
                dispatch_id: format!("dispatch-{sequence}"),
                payload_sha256: "a".repeat(64),
                state: ComputerDispatchState::Uncertain,
                prepared_at: now - Duration::seconds(2),
                injected_at: Some(now - Duration::seconds(1)),
                completed_at: Some(now),
                outcome_sha256: None,
                error_code: Some(ComputerErrorCode::UncertainOutcome),
            }),
            _ => None,
        };
        let lease = ComputerSurfaceLease {
            schema_version: COMPUTER_SURFACE_LEASE_SCHEMA_VERSION,
            lease_id: format!("lease-{sequence}"),
            work_id: format!("work-{sequence}"),
            work_attempt_id: format!("attempt-{sequence}"),
            agent_id: format!("agent-{sequence}"),
            agent_spec_revision: 1,
            run_id: format!("run-{sequence}"),
            surface: ComputerSurfaceBinding::issue(),
            authority_epoch: 1,
            control_epoch: 1,
            frame_epoch: dispatch.as_ref().map(|_| 1),
            input_domain_id: format!("input-{sequence}"),
            conflict_domain_id: format!("conflict-{sequence}"),
            revision: 1,
            expires_at: now + Duration::minutes(1),
            queue_sequence: sequence,
            priority: super::super::coordination::HostLeasePriority::Normal,
            state,
            dispatch,
            disposition: Some("retention_fixture".into()),
            created_at: now - Duration::minutes(1),
            updated_at: now,
        };
        lease.validate().unwrap();
        lease
    }

    fn ready_agent_run_for_surface_lease(
        store: &ComputerStore,
        suffix: &str,
        now: DateTime<Utc>,
    ) -> (ComputerRun, HostSurfaceLeaseRequest) {
        let domain = PhysicalInputDomain::attested("test", &format!("retention-{suffix}")).unwrap();
        let surface = store.intern_physical_domain(&domain).unwrap();
        let agent_id = format!("agent-retention-{suffix}");
        let work_id = format!("work-retention-{suffix}");
        let attempt_id = format!("attempt-retention-{suffix}");
        let proof = surface
            .stamp_proof(
                crate::computer_use::ComputerCapabilityProof::ForegroundSemantic {
                    backend_id: "test_foreground".into(),
                    observe: true,
                    semantic_actions: true,
                    text_entry: true,
                },
            )
            .unwrap();
        let mut run = ComputerRun::new_with_isolation(
            Uuid::new_v4(),
            Some(format!("/tmp/workspace-retention-{suffix}")),
            target(),
            ComputerUseLimits::default(),
            ComputerPrincipal::from_host_agent_record(&agent_id, 1).unwrap(),
            surface.binding,
            proof,
        )
        .unwrap();
        run.work_attempt = Some(crate::computer_use::ComputerWorkAttemptBinding {
            work_id: work_id.clone(),
            work_attempt_id: attempt_id.clone(),
            agent_id,
            agent_spec_revision: 1,
        });
        run.transition(ComputerRunState::Ready).unwrap();
        store.save_run(&run).unwrap();
        let request = HostSurfaceLeaseRequest {
            work_id,
            work_attempt_id: attempt_id,
            run_id: run.run_id.clone(),
            priority: super::super::coordination::HostLeasePriority::Normal,
            expires_at: now + Duration::minutes(1),
        };
        (run, request)
    }

    #[test]
    fn ordinary_terminal_surface_leases_make_room_for_new_work() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path()).unwrap();
        let now = Utc::now();
        for sequence in 1..=MAX_SURFACE_LEASES as u64 {
            let lease = terminal_surface_lease(sequence, ComputerSurfaceLeaseState::Released, now);
            store.write_surface_lease_unlocked(&lease).unwrap();
        }
        assert_eq!(
            store.list_surface_leases().unwrap().len(),
            MAX_SURFACE_LEASES
        );

        let (_, request) = ready_agent_run_for_surface_lease(&store, "new-work", now);
        let queued = store.queue_surface_lease(request, now).unwrap();
        assert_eq!(queued.state, ComputerSurfaceLeaseState::Queued);
        assert_eq!(queued.queue_sequence, MAX_SURFACE_LEASES as u64 + 1);
        let retained = store.list_surface_leases().unwrap();
        assert_eq!(retained.len(), MAX_SURFACE_LEASES);
        assert!(retained
            .iter()
            .any(|lease| lease.lease_id == queued.lease_id));
        assert!(retained.iter().all(|lease| lease.lease_id != "lease-1"));
    }

    #[test]
    fn uncertain_surface_leases_are_never_pruned_for_capacity() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path()).unwrap();
        let now = Utc::now();
        for sequence in 1..=MAX_SURFACE_LEASES as u64 {
            let lease = terminal_surface_lease(sequence, ComputerSurfaceLeaseState::Uncertain, now);
            store.write_surface_lease_unlocked(&lease).unwrap();
        }
        let (_, request) = ready_agent_run_for_surface_lease(&store, "uncertain-full", now);
        let error = store.queue_surface_lease(request, now).unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::LimitReached);
        let retained = store.list_surface_leases().unwrap();
        assert_eq!(retained.len(), MAX_SURFACE_LEASES);
        assert!(retained
            .iter()
            .all(|lease| lease.state == ComputerSurfaceLeaseState::Uncertain));
    }

    #[test]
    fn reopen_ages_out_only_replay_safe_terminal_surface_leases() {
        let dir = tempdir().unwrap();
        let old = Utc::now() - TERMINAL_SURFACE_LEASE_AGE - Duration::minutes(1);
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let released = terminal_surface_lease(1, ComputerSurfaceLeaseState::Released, old);
            let uncertain = terminal_surface_lease(2, ComputerSurfaceLeaseState::Uncertain, old);
            store.write_surface_lease_unlocked(&released).unwrap();
            store.write_surface_lease_unlocked(&uncertain).unwrap();
        }

        let store = ComputerStore::open(dir.path()).unwrap();
        assert!(store.load_surface_lease("lease-1").unwrap().is_none());
        assert_eq!(
            store.load_surface_lease("lease-2").unwrap().unwrap().state,
            ComputerSurfaceLeaseState::Uncertain
        );
    }

    #[test]
    fn restart_interrupts_run_and_clears_authority() {
        let dir = tempdir().unwrap();
        let run_id;
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let mut run = ComputerRun::attested_foreground_for_test(
                Uuid::new_v4(),
                None,
                target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
            run_id = run.run_id.clone();
            let now = Utc::now();
            run.grant = Some(ActionGrant::for_run(
                &run,
                BTreeSet::from([ActionClass::Semantic]),
                now,
                now + Duration::minutes(5),
                None,
            ));
            run.transition(ComputerRunState::Ready).unwrap();
            run.last_outcome = Some(ActionOutcome::bounded(
                "PRIVATE_DOCUMENT_TITLE leaked from AX",
                Some(true),
            ));
            store.save_run(&run).unwrap();
        }
        let store = ComputerStore::open(dir.path()).unwrap();
        let recovered = store.load_run(&run_id).unwrap().unwrap();
        assert_eq!(recovered.state, ComputerRunState::Interrupted);
        assert_eq!(
            recovered.control_disposition,
            ComputerControlDisposition::Interrupted
        );
        assert!(recovered.control_epoch > 0);
        assert!(recovered.grant.is_none());
        assert!(recovered.current_observation.is_none());
        assert!(
            recovered.last_outcome.is_none(),
            "restart must not keep a leaky last_outcome"
        );
        assert_eq!(
            recovered.last_error.as_ref().map(|error| error.code),
            Some(ComputerErrorCode::Interrupted)
        );
        let last = recovered.audit.last().expect("recovery is journaled");
        assert_eq!(last.operation, "recover");
        assert_eq!(last.disposition, "interrupted");
        assert_eq!(last.error_code, Some(ComputerErrorCode::Interrupted));
    }

    #[test]
    fn restart_rotates_incarnation_coerces_isolated_proof_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let run_id;
        let original_incarnation;
        let original_surface_id;
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let surface = crate::computer_use::ComputerSurfaceBinding::issue();
            original_incarnation = surface.incarnation.clone();
            original_surface_id = surface.surface_id.clone();
            let owner = Uuid::new_v4();
            let mut run = ComputerRun::new_with_isolation(
                owner,
                None,
                target(),
                ComputerUseLimits::default(),
                crate::computer_use::ComputerPrincipal::local_operator(owner),
                surface.clone(),
                crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
                    backend_id: crate::computer_use::SIMULATOR_ISOLATED_BACKEND_ID.into(),
                    surface_id: surface.surface_id.clone(),
                    incarnation: surface.incarnation.clone(),
                    input_domain_id: uuid::Uuid::new_v4().to_string(),
                    origin: crate::computer_use::IsolationProofOrigin::SimulatorFixture,
                    observe: true,
                    semantic_actions: true,
                    text_entry: true,
                    key_chords: true,
                    pointer_fallback: true,
                },
            )
            .unwrap();
            run_id = run.run_id.clone();
            let now = Utc::now();
            run.grant = Some(ActionGrant::for_run(
                &run,
                BTreeSet::from([ActionClass::Semantic]),
                now,
                now + Duration::minutes(5),
                None,
            ));
            run.transition(ComputerRunState::Ready).unwrap();
            store.save_run(&run).unwrap();
        }

        let first = ComputerStore::open(dir.path())
            .unwrap()
            .load_run(&run_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.state, ComputerRunState::Interrupted);
        assert!(first.grant.is_none());
        assert_eq!(first.surface.surface_id, original_surface_id);
        assert_ne!(first.surface.incarnation, original_incarnation);
        assert_eq!(
            first.capability_proof,
            crate::computer_use::ComputerCapabilityProof::Unproven
        );
        assert_eq!(first.freshness_tick, 0);
        assert!(first.authority_epoch > 0);
        let first_incarnation = first.surface.incarnation.clone();
        let first_epoch = first.authority_epoch;
        let first_version = first.version;

        let second = ComputerStore::open(dir.path())
            .unwrap()
            .load_run(&run_id)
            .unwrap()
            .unwrap();
        assert_eq!(second.state, ComputerRunState::Interrupted);
        assert!(second.grant.is_none());
        assert_eq!(second.surface.incarnation, first_incarnation);
        assert_eq!(second.authority_epoch, first_epoch);
        assert_eq!(second.version, first_version);
        assert_eq!(
            second.capability_proof,
            crate::computer_use::ComputerCapabilityProof::Unproven
        );
    }

    fn test_stamp() -> MutationStamp {
        MutationStamp::from_caller(
            crate::computer_use::ComputerPrincipal::local_operator(Uuid::new_v4()),
            None,
        )
    }

    #[test]
    fn interrupted_claim_becomes_uncertain_not_replayable() {
        let dir = tempdir().unwrap();
        let payload_hash = "a".repeat(64);
        let stamp = test_stamp();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            assert!(matches!(
                store
                    .claim_mutation("request-1", "act", &payload_hash, &stamp)
                    .unwrap(),
                MutationClaim::Perform
            ));
        }
        let store = ComputerStore::open(dir.path()).unwrap();
        assert!(matches!(
            store
                .claim_mutation("request-1", "act", &payload_hash, &stamp)
                .unwrap(),
            MutationClaim::Uncertain
        ));
        assert_eq!(
            store
                .claim_mutation("request-1", "act", "other", &stamp)
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        let other = MutationStamp::from_caller(
            crate::computer_use::ComputerPrincipal::local_operator(Uuid::new_v4()),
            None,
        );
        assert_eq!(
            store
                .claim_mutation("request-1", "act", &payload_hash, &other)
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn unstamped_legacy_receipt_fails_closed() {
        let dir = tempdir().unwrap();
        let payload_hash = "b".repeat(64);
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.receipt_path("legacy").unwrap();
            fs::write(
                path,
                serde_json::to_vec(&serde_json::json!({
                    "requestId": "legacy",
                    "operation": "observe",
                    "payloadHash": payload_hash,
                    "state": "succeeded",
                    "createdAt": Utc::now(),
                    "updatedAt": Utc::now(),
                    "result": {"ok": true}
                }))
                .unwrap(),
            )
            .unwrap();
        }
        let store = ComputerStore::open(dir.path()).unwrap();
        assert!(matches!(
            store
                .claim_mutation("legacy", "observe", &payload_hash, &test_stamp())
                .unwrap(),
            MutationClaim::Uncertain
        ));
    }

    #[test]
    fn corrupt_json_fails_store_open_closed() {
        let dir = tempdir().unwrap();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.root().join("runs").join("corrupt.json");
            fs::write(path, b"{").unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
    }

    #[test]
    fn contradictory_legacy_records_reopen_as_interrupted_unproven() {
        let owner = Uuid::new_v4();
        let surface_a = crate::computer_use::ComputerSurfaceBinding::issue();
        let surface_b = crate::computer_use::ComputerSurfaceBinding::issue();
        let now = Utc::now();
        let cases: &[(&str, serde_json::Value)] = &[
            (
                "cross-surface-isolated",
                serde_json::json!({
                    "runId": "cross-surface-isolated",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "surface": surface_a,
                    "initiatingPrincipal": {
                        "kind": "local_operator_session",
                        "sessionId": owner
                    },
                    "capabilityProof": {
                        "kind": "independently_isolated_visual_input_domain",
                        "backendId": crate::computer_use::SIMULATOR_ISOLATED_BACKEND_ID,
                        "surfaceId": surface_b.surface_id,
                        "incarnation": surface_b.incarnation,
                        "inputDomainId": Uuid::new_v4().to_string(),
                        "origin": "simulator_fixture",
                        "observe": true,
                        "semanticActions": true,
                        "textEntry": true,
                        "keyChords": true,
                        "pointerFallback": true
                    }
                }),
            ),
            (
                "agent-principal",
                serde_json::json!({
                    "runId": "agent-principal",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "initiatingPrincipal": {
                        "kind": "agent",
                        "agentId": format!("agent-{}", Uuid::new_v4()),
                        "specRevision": 1
                    },
                    "capabilityProof": { "kind": "unproven" }
                }),
            ),
            (
                "missing-schema",
                serde_json::json!({
                    "runId": "missing-schema",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": []
                }),
            ),
            (
                "owner-mismatch-principal",
                serde_json::json!({
                    "runId": "owner-mismatch-principal",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "schemaVersion": 1,
                    "initiatingPrincipal": {
                        "kind": "local_operator_session",
                        "sessionId": Uuid::new_v4()
                    },
                    "capabilityProof": { "kind": "unproven" }
                }),
            ),
            (
                "zero-frame-observation",
                serde_json::json!({
                    "runId": "zero-frame-observation",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "schemaVersion": 1,
                    "surface": surface_a,
                    "initiatingPrincipal": {
                        "kind": "local_operator_session",
                        "sessionId": owner
                    },
                    "capabilityProof": {
                        "kind": "foreground_semantic",
                        "backendId": "test_foreground",
                        "observe": true,
                        "semanticActions": true,
                        "textEntry": true
                    },
                    "currentObservation": {
                        "observationId": "obs-1",
                        "sequence": 1,
                        "target": {
                            "appId": "com.grokptah.demo",
                            "windowId": "main",
                            "generation": 1,
                            "displayName": "Demo",
                            "sensitivity": "none"
                        },
                        "capturedAt": now,
                        "geometry": {
                            "x": 0.0,
                            "y": 0.0,
                            "width": 800.0,
                            "height": 600.0,
                            "scaleFactor": 1.0
                        },
                        "elements": [],
                        "elementsTruncated": false,
                        "sensitivity": "none",
                        "authority": {
                            "surface": surface_a,
                            "frameEpoch": 0,
                            "targetGeneration": 1,
                            "authorityEpoch": 0,
                            "controlEpoch": 0,
                            "freshness": {
                                "surfaceId": surface_a.surface_id,
                                "incarnation": surface_a.incarnation,
                                "tick": 1
                            }
                        }
                    }
                }),
            ),
        ];

        for (name, record) in cases {
            let dir = tempdir().unwrap();
            {
                let store = ComputerStore::open(dir.path()).unwrap();
                let path = store.run_path(name).unwrap();
                fs::write(path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
            }
            let store = ComputerStore::open(dir.path()).unwrap();
            let recovered = store.load_run(name).unwrap().unwrap();
            assert_eq!(
                recovered.state,
                ComputerRunState::Interrupted,
                "{name} must reopen interrupted"
            );
            assert!(recovered.grant.is_none(), "{name} must drop grant");
            assert!(
                recovered.current_observation.is_none(),
                "{name} must drop observation"
            );
            if *name != "zero-frame-observation" {
                assert_eq!(
                    recovered.capability_proof,
                    crate::computer_use::ComputerCapabilityProof::Unproven,
                    "{name} must be unproven"
                );
            }
            if *name == "agent-principal" || *name == "owner-mismatch-principal" {
                assert!(recovered.initiating_principal.is_none());
            }
        }
    }

    #[test]
    fn mismatched_record_identity_fails_store_open_closed() {
        let dir = tempdir().unwrap();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let run =
                ComputerRun::new(Uuid::new_v4(), None, target(), ComputerUseLimits::default())
                    .unwrap();
            store.save_run(&run).unwrap();
            fs::rename(
                store.run_path(&run.run_id).unwrap(),
                store.run_path("different-run").unwrap(),
            )
            .unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
    }

    #[test]
    fn oversized_record_fails_before_deserialization() {
        let dir = tempdir().unwrap();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.root().join("runs").join("oversized.json");
            let file = fs::File::create(path).unwrap();
            file.set_len(MAX_RECORD_BYTES + 1).unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
    }

    #[test]
    fn unknown_future_run_schema_fails_store_open_and_is_not_rewritten() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        let persisted_path = {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.run_path("future-run").unwrap();
            fs::write(
                &path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "runId": "future-run",
                    "ownerSessionId": Uuid::new_v4(),
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "schemaVersion": 99,
                    "capabilityProof": {
                        "kind": "foreground_semantic",
                        "backendId": "future_backend",
                        "observe": true,
                        "semanticActions": true,
                        "textEntry": true
                    }
                }))
                .unwrap(),
            )
            .unwrap();
            path
        };
        assert!(ComputerStore::open(dir.path()).is_err());
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&persisted_path).unwrap()).unwrap();
        assert_eq!(persisted["schemaVersion"], 99);
        assert_eq!(persisted["capabilityProof"]["kind"], "foreground_semantic");
    }

    #[test]
    fn unknown_future_receipt_schema_fails_store_open() {
        let dir = tempdir().unwrap();
        let payload_hash = "c".repeat(64);
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.receipt_path("future-receipt").unwrap();
            fs::write(
                path,
                serde_json::to_vec(&serde_json::json!({
                    "requestId": "future-receipt",
                    "operation": "act",
                    "payloadHash": payload_hash,
                    "state": "succeeded",
                    "createdAt": Utc::now(),
                    "updatedAt": Utc::now(),
                    "result": {"ok": true},
                    "schemaVersion": 99,
                    "callerKind": "local_operator_session",
                    "preAuthorityEpoch": 0,
                    "preControlEpoch": 0
                }))
                .unwrap(),
            )
            .unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
    }
}
