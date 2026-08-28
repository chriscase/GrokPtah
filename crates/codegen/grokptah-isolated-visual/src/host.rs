//! The single host-owned authority for isolated Computer Use.
//!
//! # One authority
//!
//! Exactly one component owns leases, revisions, dispatch de-duplication, and
//! cleanup receipts: this type. Helper launch, dispatch, cancel, crash,
//! expiry, and restart are all fenced here, against durable records. There is
//! deliberately no second, helper-local state machine keeping its own lease id,
//! its own `used` dispatch map, and its own receipt: two authorities can
//! disagree, and when they do neither can be trusted about whether physical
//! input reached the guest.
//!
//! # Durability is part of the contract
//!
//! `Prepared` and `Injected` are written durably *before* the corresponding
//! real-world step. If the `Injected` write fails, injection does not happen
//! and the dispatch is refused as known-not-injected. If a write fails after
//! injection, the outcome is `Uncertain` and is never replayed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::cleanup::{
    CleanupProbe, CleanupReceipt, IsolatedCleanupReason, ResourceProbeResult, ResourceState,
};
use crate::clock::HostClock;
use crate::code_identity::SystemCodeIdentityProbe;
use crate::error::{IsolatedError, IsolatedErrorCode, IsolatedResult};
use crate::ids::{
    isolated_conflict_domain_id, isolated_input_domain_id, sha256_hex, validate_id, SCHEMA_VERSION,
};
use crate::lease::{
    attempt_has_active_lease, domain_has_capacity, ComputerDispatchRecord, ComputerDispatchState,
    ComputerSurfaceLease, ComputerSurfaceLeaseState, HostLeasePriority, MAX_LEASE_LIFETIME,
};
use crate::lifecycle::{
    IsolatedEvidenceClass, IsolatedGuestPhase, IsolatedGuestRecord, IsolatedGuestTerminal,
};
use crate::manifest::{
    ComputerSurfaceBinding, HelperIdentity, IsolatedSourceManifest, IsolatedVisualResourceLimits,
    MAX_CONCURRENT_GUESTS, MAX_SURFACE_LEASES,
};
use crate::occupancy::{resource_key, OccupancyRecord, OccupancyState, OccupancyStore};
use crate::preflight::IsolatedPreflight;
use crate::projection::{project_guest, IsolatedVisualProjection};
use crate::protocol::{
    mac_frame, verify_frame_mac, IsolatedFrameMeta, IsolatedInputEvent, ResidentFrame,
    CHANNEL_SECRET_BYTES,
};
use crate::resolver::{HermeticResolver, ResolvedSource};
use crate::simulator::IsolatedSimulator;
use crate::store::IsolatedVisualStore;
use crate::trust_root::PackagedTrustRoot;

pub struct IsolatedVisualHost {
    store: IsolatedVisualStore,
    clock: Arc<dyn HostClock>,
    resolver: HermeticResolver,
    simulator: IsolatedSimulator,
    secrets: BTreeMap<String, [u8; CHANNEL_SECRET_BYTES]>,
    preflight: IsolatedPreflight,
    next_queue_sequence: u64,
    occupancy: OccupancyStore,
    /// Resource keys by guest. Bookkeeping only: cleanup receipts re-derive
    /// occupancy from the store on disk, not from this map.
    occupancy_keys: BTreeMap<String, String>,
}

pub struct CreateGuestRequest {
    pub run_id: String,
    pub work_id: String,
    pub work_attempt_id: String,
    pub agent_id: String,
    pub agent_spec_revision: u64,
    pub helper: HelperIdentity,
    pub source: IsolatedSourceManifest,
    pub limits: IsolatedVisualResourceLimits,
}

impl IsolatedVisualHost {
    pub fn open(
        root: impl AsRef<Path>,
        clock: Arc<dyn HostClock>,
        resolver: HermeticResolver,
    ) -> IsolatedResult<Self> {
        Self::open_with_artifacts(
            root,
            clock,
            resolver,
            std::env::var_os("GROKPTAH_ISOLATED_VISUAL_ARTIFACT_ROOT")
                .map(PathBuf::from)
                .as_deref(),
        )
    }

    pub fn open_with_artifacts(
        root: impl AsRef<Path>,
        clock: Arc<dyn HostClock>,
        resolver: HermeticResolver,
        artifact_root: Option<&Path>,
    ) -> IsolatedResult<Self> {
        let trust_root = PackagedTrustRoot::from_env(artifact_root);
        let preflight = IsolatedPreflight::inspect(
            artifact_root,
            trust_root.as_ref().ok(),
            &SystemCodeIdentityProbe,
            trust_root.as_ref().err().map(|error| error.message.clone()),
        );
        Self::open_with_preflight(root, clock, resolver, preflight)
    }

    /// Open with a preflight the caller already computed. Used by tests to
    /// exercise the authority without an admitted packaged artifact; it never
    /// upgrades evidence class, so a simulator host still cannot claim
    /// Virtualization.framework.
    pub fn open_with_preflight(
        root: impl AsRef<Path>,
        clock: Arc<dyn HostClock>,
        resolver: HermeticResolver,
        preflight: IsolatedPreflight,
    ) -> IsolatedResult<Self> {
        let now = clock.now();
        let store = IsolatedVisualStore::open(root, now)?;
        let occupancy = OccupancyStore::open(store.root().join("occupancy"))?;
        let mut host = Self {
            store,
            clock,
            resolver,
            simulator: IsolatedSimulator::new(),
            secrets: BTreeMap::new(),
            preflight,
            next_queue_sequence: 1,
            occupancy,
            occupancy_keys: BTreeMap::new(),
        };
        host.next_queue_sequence = host
            .store
            .list_leases()?
            .into_iter()
            .map(|lease| lease.queue_sequence)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        for dir in [host.overlay_dir(), host.helper_dir(), host.channel_dir()] {
            fs::create_dir_all(&dir).map_err(|error| IsolatedError::internal(error.to_string()))?;
        }
        Ok(host)
    }

    pub fn preflight(&self) -> &IsolatedPreflight {
        &self.preflight
    }

    /// Durable root of this host's store.
    pub fn store_root(&self) -> &Path {
        self.store.root()
    }

    /// What the last store open had to repair (quarantines, reaped grants,
    /// dispatches carried to Uncertain).
    pub fn recovery(&self) -> &crate::store::RecoveryReport {
        self.store.recovery()
    }

    /// Directory holding the per-guest overlay files.
    fn overlay_dir(&self) -> PathBuf {
        self.store.root().join("overlays")
    }

    /// Directory holding one liveness marker per launched helper.
    fn helper_dir(&self) -> PathBuf {
        self.store.root().join("helpers")
    }

    /// Directory holding one marker per live channel incarnation.
    fn channel_dir(&self) -> PathBuf {
        self.store.root().join("channels")
    }

    fn overlay_path(&self, guest_id: &str) -> PathBuf {
        self.overlay_dir().join(format!("{guest_id}.overlay"))
    }

    fn helper_marker_path(&self, guest_id: &str) -> PathBuf {
        self.helper_dir().join(format!("{guest_id}.helper"))
    }

    fn channel_marker_path(&self, incarnation: &str) -> PathBuf {
        self.channel_dir().join(format!("{incarnation}.channel"))
    }

    pub fn resolver_mut(&mut self) -> &mut HermeticResolver {
        &mut self.resolver
    }

    pub fn resolve_source(
        &self,
        manifest: &IsolatedSourceManifest,
        staging: &Path,
    ) -> IsolatedResult<ResolvedSource> {
        self.resolver.resolve(manifest, staging)
    }

    pub fn create_guest(
        &mut self,
        request: CreateGuestRequest,
    ) -> IsolatedResult<IsolatedGuestRecord> {
        request.source.validate()?;
        request.limits.validate()?;
        request.helper.validate()?;
        validate_id("run_id", &request.run_id)?;
        validate_id("work_id", &request.work_id)?;
        validate_id("work_attempt_id", &request.work_attempt_id)?;
        validate_id("agent_id", &request.agent_id)?;
        let live = self
            .store
            .list_guests()?
            .into_iter()
            .filter(IsolatedGuestRecord::is_live)
            .count();
        if live >= MAX_CONCURRENT_GUESTS {
            return Err(IsolatedError::limit(
                "isolated guest concurrency budget exhausted",
            ));
        }
        if self
            .store
            .list_guests()?
            .iter()
            .any(|other| other.is_live() && other.work_attempt_id == request.work_attempt_id)
        {
            return Err(IsolatedError::conflict(
                "work attempt already has a live isolated guest",
            ));
        }
        let now = self.clock.now();
        let guest_id = Uuid::new_v4().to_string();
        let guest = IsolatedGuestRecord {
            schema_version: SCHEMA_VERSION,
            guest_id: guest_id.clone(),
            run_id: request.run_id,
            work_id: request.work_id,
            work_attempt_id: request.work_attempt_id,
            agent_id: request.agent_id,
            agent_spec_revision: request.agent_spec_revision,
            helper: request.helper,
            surface: ComputerSurfaceBinding::issue(),
            input_domain_id: isolated_input_domain_id(&guest_id),
            conflict_domain_id: isolated_conflict_domain_id(&guest_id),
            source: request.source,
            packaged_manifest: None,
            phase: IsolatedGuestPhase::Create,
            terminal: None,
            cleaned: false,
            evidence_class: IsolatedEvidenceClass::SimulatorIneligible,
            limits: request.limits,
            frame_epoch: 0,
            frames_seen: 0,
            input_events_seen: 0,
            resident_frame_bytes: 0,
            captured_bytes: 0,
            created_at: now,
            updated_at: now,
            started_at: None,
            ended_at: None,
            disposition: None,
        };
        guest.validate()?;
        let key = resource_key(
            &guest.helper.content_sha256,
            &guest.guest_id,
            &guest.surface.incarnation,
        );
        self.occupancy.try_acquire(OccupancyRecord {
            schema_version: SCHEMA_VERSION,
            resource_key: key.clone(),
            owner_id: guest.agent_id.clone(),
            guest_id: guest.guest_id.clone(),
            surface_incarnation: guest.surface.incarnation.clone(),
            image_digest: guest.helper.content_sha256.clone(),
            overlay_id: guest.guest_id.clone(),
            vm_instance_id: None,
            state: OccupancyState::Clear,
            updated_at: now,
        })?;
        // Each resource gets a durable on-disk marker. Cleanup receipts are
        // re-derived from these files, so teardown cannot make a resource look
        // released merely by dropping an in-memory key.
        for dir in [self.overlay_dir(), self.helper_dir(), self.channel_dir()] {
            fs::create_dir_all(&dir).map_err(|error| IsolatedError::internal(error.to_string()))?;
        }
        write_marker(
            &self.overlay_path(&guest.guest_id),
            guest.guest_id.as_bytes(),
        )?;
        write_marker(
            &self.helper_marker_path(&guest.guest_id),
            guest.helper.helper_id.as_bytes(),
        )?;
        write_marker(
            &self.channel_marker_path(&guest.surface.incarnation),
            guest.surface.incarnation.as_bytes(),
        )?;
        self.store.save_guest(&guest)?;
        self.simulator.attach(&guest.guest_id);
        self.secrets.insert(
            guest.surface.incarnation.clone(),
            fresh_secret(&guest.guest_id, now),
        );
        self.occupancy_keys.insert(guest.guest_id.clone(), key);
        Ok(guest)
    }

    pub fn mark_ready(&mut self, guest_id: &str) -> IsolatedResult<IsolatedGuestRecord> {
        let mut guest = self.require_live_guest(guest_id)?;
        let now = self.clock.now();
        guest.transition(IsolatedGuestPhase::Ready, now)?;
        self.store.save_guest(&guest)?;
        Ok(guest)
    }

    pub fn mark_running(&mut self, guest_id: &str) -> IsolatedResult<IsolatedGuestRecord> {
        let mut guest = self.require_live_guest(guest_id)?;
        let now = self.clock.now();
        guest.transition(IsolatedGuestPhase::Running, now)?;
        self.store.save_guest(&guest)?;
        Ok(guest)
    }

    pub fn enqueue_lease(&mut self, guest_id: &str) -> IsolatedResult<ComputerSurfaceLease> {
        let guest = self.require_live_guest(guest_id)?;
        if guest.phase == IsolatedGuestPhase::Closing {
            return Err(IsolatedError::invalid_state(
                "closing guest cannot acquire a lease",
            ));
        }
        let leases = self.store.list_leases()?;
        if leases.len() >= MAX_SURFACE_LEASES {
            return Err(IsolatedError::limit("surface lease record limit reached"));
        }
        if attempt_has_active_lease(&leases, &guest.work_attempt_id) {
            return Err(IsolatedError::conflict(
                "work attempt already holds an active surface lease",
            ));
        }
        if leases
            .iter()
            .any(|lease| lease.guest_id == guest.guest_id && !lease.state.is_terminal())
        {
            return Err(IsolatedError::conflict(
                "isolated guest is already leased to an agent",
            ));
        }
        let now = self.clock.now();
        let lease = ComputerSurfaceLease {
            schema_version: SCHEMA_VERSION,
            lease_id: Uuid::new_v4().to_string(),
            guest_id: guest.guest_id.clone(),
            work_id: guest.work_id.clone(),
            work_attempt_id: guest.work_attempt_id.clone(),
            agent_id: guest.agent_id.clone(),
            agent_spec_revision: guest.agent_spec_revision,
            run_id: guest.run_id.clone(),
            surface: guest.surface.clone(),
            authority_epoch: 1,
            control_epoch: 1,
            frame_epoch: None,
            input_domain_id: guest.input_domain_id.clone(),
            conflict_domain_id: guest.conflict_domain_id.clone(),
            revision: 1,
            expires_at: now + Duration::minutes(5),
            queue_sequence: self.next_queue_sequence,
            priority: HostLeasePriority::Normal,
            state: ComputerSurfaceLeaseState::Queued,
            dispatch: None,
            disposition: None,
            created_at: now,
            updated_at: now,
        };
        if lease.expires_at > now + MAX_LEASE_LIFETIME {
            return Err(IsolatedError::invalid(
                "lease lifetime exceeds the host ceiling",
            ));
        }
        lease.validate()?;
        self.next_queue_sequence = self.next_queue_sequence.saturating_add(1);
        self.store.save_lease(&lease)?;
        Ok(lease)
    }

    pub fn grant_next(&mut self, conflict_domain_id: &str) -> IsolatedResult<ComputerSurfaceLease> {
        validate_id("conflict_domain_id", conflict_domain_id)?;
        let now = self.clock.now();
        // Expired grants are reaped before capacity is judged, so a lapsed
        // lease can neither hold a conflict domain nor be granted again.
        self.store.reap_expired(now)?;
        let mut leases = self.store.list_leases()?;
        if !domain_has_capacity(&leases, conflict_domain_id) {
            return Err(IsolatedError::conflict(
                "conflict domain already has a granted or dispatching lease",
            ));
        }
        let newest = leases
            .iter()
            .map(|lease| lease.queue_sequence)
            .max()
            .unwrap_or(0);
        let mut queued: Vec<_> = leases
            .iter_mut()
            .filter(|lease| {
                lease.conflict_domain_id == conflict_domain_id
                    && lease.state == ComputerSurfaceLeaseState::Queued
                    && lease.expires_at > now
            })
            .collect();
        queued.sort_by(|a, b| {
            b.effective_priority(newest)
                .cmp(&a.effective_priority(newest))
                .then(a.queue_sequence.cmp(&b.queue_sequence))
        });
        let Some(lease) = queued.into_iter().next() else {
            return Err(IsolatedError::invalid_state(
                "no queued lease for conflict domain",
            ));
        };
        lease.transition(ComputerSurfaceLeaseState::Granted, now, None)?;
        self.store.save_lease(lease)?;
        Ok(lease.clone())
    }

    pub fn ingest_frame(
        &mut self,
        guest_id: &str,
        lease_id: &str,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> IsolatedResult<IsolatedFrameMeta> {
        let mut guest = self.require_live_guest(guest_id)?;
        if guest.phase != IsolatedGuestPhase::Running {
            return Err(IsolatedError::invalid_state(
                "frames require a running guest",
            ));
        }
        if duration_exceeded(&guest, self.clock.now()) {
            guest.terminate(
                IsolatedGuestTerminal::Failed,
                self.clock.now(),
                "duration_seconds exhausted",
            )?;
            self.store.save_guest(&guest)?;
            return Err(IsolatedError::limit("isolated visual duration exhausted"));
        }
        let mut lease = self.require_matching_lease(&guest, lease_id)?;
        if lease.state != ComputerSurfaceLeaseState::Granted
            && lease.state != ComputerSurfaceLeaseState::Dispatching
        {
            return Err(IsolatedError::unauthorized("lease cannot ingest a frame"));
        }
        if guest.frames_seen >= guest.limits.max_frames {
            guest.terminate(
                IsolatedGuestTerminal::Failed,
                self.clock.now(),
                "max_frames exhausted",
            )?;
            self.store.save_guest(&guest)?;
            return Err(IsolatedError::limit("isolated visual max_frames exhausted"));
        }
        let now = self.clock.now();
        guest.frame_epoch = guest.frame_epoch.saturating_add(1);
        let rotated = self.simulator.rotate_out(&guest.guest_id);
        guest.resident_frame_bytes = guest.resident_frame_bytes.saturating_sub(rotated);
        let mut meta = IsolatedFrameMeta {
            frame_id: Uuid::new_v4().to_string(),
            guest_id: guest.guest_id.clone(),
            surface_id: guest.surface.surface_id.clone(),
            incarnation: guest.surface.incarnation.clone(),
            lease_id: lease.lease_id.clone(),
            lease_revision: lease.revision,
            frame_epoch: guest.frame_epoch,
            sequence: u64::from(guest.frames_seen) + 1,
            width,
            height,
            content_sha256: sha256_hex(bytes),
            encoded_bytes: bytes.len() as u64,
            mac_sha256: "0".repeat(64),
            captured_at: now,
        };
        meta.validate(&guest.limits)?;
        let secret = self
            .secrets
            .get(&guest.surface.incarnation)
            .ok_or_else(|| IsolatedError::unauthorized("channel secret for incarnation is gone"))?;
        meta.mac_sha256 = mac_frame(secret, &meta)?;
        verify_frame_mac(secret, &meta)?;
        if guest.captured_bytes.saturating_add(meta.encoded_bytes) > guest.limits.max_captured_bytes
        {
            return Err(IsolatedError::new(
                IsolatedErrorCode::LimitReached,
                "throughput capture budget exhausted; surface degraded without terminal run failure",
            ));
        }
        let resident = ResidentFrame::new(meta.clone(), bytes.to_vec())?;
        guest.resident_frame_bytes = self.simulator.ingest_frame(
            &guest.guest_id,
            resident,
            guest.limits.max_resident_frame_bytes,
        )?;
        guest.captured_bytes = guest.captured_bytes.saturating_add(meta.encoded_bytes);
        guest.frames_seen = guest.frames_seen.saturating_add(1);
        guest.updated_at = now;
        lease.frame_epoch = Some(guest.frame_epoch);
        lease.updated_at = now;
        self.store.save_guest(&guest)?;
        self.store.save_lease(&lease)?;
        Ok(meta)
    }

    pub fn prepare_dispatch(
        &mut self,
        guest_id: &str,
        lease_id: &str,
        event: IsolatedInputEvent,
    ) -> IsolatedResult<ComputerSurfaceLease> {
        self.store.reap_expired(self.clock.now())?;
        let guest = self.require_live_guest(guest_id)?;
        event.validate(&guest.limits)?;
        if event.guest_id != guest.guest_id
            || event.surface_id != guest.surface.surface_id
            || event.incarnation != guest.surface.incarnation
        {
            return Err(IsolatedError::unauthorized(
                "input event identity does not match the live guest incarnation",
            ));
        }
        let mut lease = self.require_matching_lease(&guest, lease_id)?;
        if event.lease_id != lease.lease_id || event.lease_revision != lease.revision {
            return Err(IsolatedError::stale("input event lease revision is stale"));
        }
        if event.frame_epoch != guest.frame_epoch || lease.frame_epoch != Some(guest.frame_epoch) {
            return Err(IsolatedError::stale("input event frame is stale"));
        }
        if lease.state != ComputerSurfaceLeaseState::Granted {
            if let Some(existing) = &lease.dispatch {
                if existing.dispatch_id == event.dispatch_id {
                    if existing.payload_sha256 != event.payload_sha256()? {
                        return Err(IsolatedError::conflict(
                            "dispatch_id was reused with a different payload",
                        ));
                    }
                    return Ok(lease);
                }
            }
            return Err(IsolatedError::invalid_state(
                "lease is not granted for dispatch",
            ));
        }
        if guest.input_events_seen >= guest.limits.max_input_events {
            return Err(IsolatedError::limit(
                "isolated visual max_input_events exhausted",
            ));
        }
        let now = self.clock.now();
        if now >= lease.expires_at {
            lease.transition(
                ComputerSurfaceLeaseState::Revoked,
                now,
                Some("lease expired"),
            )?;
            self.store.save_lease(&lease)?;
            return Err(IsolatedError::unauthorized("surface lease expired"));
        }
        lease.transition(ComputerSurfaceLeaseState::Dispatching, now, None)?;
        lease.dispatch = Some(ComputerDispatchRecord {
            schema_version: SCHEMA_VERSION,
            dispatch_id: event.dispatch_id.clone(),
            payload_sha256: event.payload_sha256()?,
            state: ComputerDispatchState::Prepared,
            prepared_at: now,
            injected_at: None,
            completed_at: None,
            outcome_sha256: None,
            error_code: None,
        });
        lease.validate()?;
        self.store.save_lease(&lease)?;
        Ok(lease)
    }

    /// Persist Injected then apply. A crash after this persist is recovered as
    /// Uncertain and never replayed.
    pub fn inject_dispatch(
        &mut self,
        guest_id: &str,
        lease_id: &str,
        event: IsolatedInputEvent,
        crash_after_inject: bool,
    ) -> IsolatedResult<ComputerSurfaceLease> {
        let mut lease = self.prepare_inject_state(guest_id, lease_id, &event)?;
        if let Some(dispatch) = &lease.dispatch {
            if dispatch.dispatch_id == event.dispatch_id {
                match dispatch.state {
                    ComputerDispatchState::Acknowledged => return Ok(lease),
                    ComputerDispatchState::Prepared => {}
                    ComputerDispatchState::KnownNotInjected => {
                        return Err(IsolatedError::new(
                            IsolatedErrorCode::Interrupted,
                            "dispatch was known-not-injected and will not be replayed",
                        ));
                    }
                    ComputerDispatchState::Injected
                    | ComputerDispatchState::Uncertain
                    | ComputerDispatchState::Failed => {
                        return Err(IsolatedError::new(
                            IsolatedErrorCode::UncertainOutcome,
                            "dispatch was already injected or uncertain and will not be replayed",
                        ));
                    }
                }
            }
        }
        let now = self.clock.now();
        {
            let dispatch = lease.dispatch.as_mut().expect("prepared dispatch");
            dispatch.state = ComputerDispatchState::Injected;
            dispatch.injected_at = Some(now);
        }
        lease.updated_at = now;
        lease.validate()?;
        // The Injected write must land before anything is injected. If the
        // ledger cannot be made durable, refuse: nothing was injected, so the
        // dispatch is known-not-injected rather than uncertain.
        if let Err(write_error) = self.store.save_lease(&lease) {
            let mut refused =
                self.require_matching_lease(&self.require_guest(guest_id)?, lease_id)?;
            if let Some(dispatch) = refused.dispatch.as_mut() {
                dispatch.state = ComputerDispatchState::KnownNotInjected;
                dispatch.injected_at = None;
                dispatch.completed_at = Some(now);
                dispatch.error_code = Some(IsolatedErrorCode::Internal);
            }
            let _ = refused.transition(
                ComputerSurfaceLeaseState::Revoked,
                now,
                Some("durable Injected write failed; dispatch refused"),
            );
            let _ = self.store.save_lease(&refused);
            return Err(IsolatedError::new(
                IsolatedErrorCode::Internal,
                format!(
                    "dispatch refused: the Injected ledger write is not durable ({})",
                    write_error.message
                ),
            ));
        }
        if crash_after_inject {
            return Ok(lease);
        }
        if let Err(error) = self.simulator.accept_input(event.clone()) {
            let now = self.clock.now();
            if let Some(dispatch) = lease.dispatch.as_mut() {
                dispatch.state = ComputerDispatchState::Uncertain;
                dispatch.completed_at = Some(now);
                dispatch.error_code = Some(IsolatedErrorCode::UncertainOutcome);
            }
            lease.state = ComputerSurfaceLeaseState::Uncertain;
            lease.revision = lease.revision.saturating_add(1);
            lease.updated_at = now;
            lease.disposition =
                Some("injector error after Injected; physical delivery uncertain".into());
            self.store.save_lease(&lease)?;
            return Err(IsolatedError::new(
                IsolatedErrorCode::UncertainOutcome,
                error.to_string(),
            ));
        }
        let mut guest = self.require_live_guest(guest_id)?;
        guest.input_events_seen = guest.input_events_seen.saturating_add(1);
        guest.updated_at = now;
        self.store.save_guest(&guest)?;
        {
            let dispatch = lease.dispatch.as_mut().expect("injected dispatch");
            dispatch.state = ComputerDispatchState::Acknowledged;
            dispatch.completed_at = Some(now);
            dispatch.outcome_sha256 = Some(sha256_hex(b"ack"));
        }
        lease.transition(
            ComputerSurfaceLeaseState::Released,
            now,
            Some("acknowledged"),
        )?;
        // Injection already happened. If the acknowledgement is not durable we
        // cannot claim success, and we must not replay: the outcome is
        // uncertain and stays that way.
        if let Err(write_error) = self.store.save_lease(&lease) {
            return Err(IsolatedError::new(
                IsolatedErrorCode::UncertainOutcome,
                format!(
                    "input was injected but the acknowledgement is not durable ({}); \
                     the outcome is uncertain and will not be replayed",
                    write_error.message
                ),
            ));
        }
        Ok(lease)
    }

    fn prepare_inject_state(
        &mut self,
        guest_id: &str,
        lease_id: &str,
        event: &IsolatedInputEvent,
    ) -> IsolatedResult<ComputerSurfaceLease> {
        let guest = self.require_live_guest(guest_id)?;
        let lease = self.require_matching_lease(&guest, lease_id)?;
        if let Some(dispatch) = &lease.dispatch {
            if dispatch.dispatch_id == event.dispatch_id {
                let expected = event.payload_sha256()?;
                if dispatch.payload_sha256 != expected {
                    return Err(IsolatedError::conflict(
                        "dispatch_id was reused with a different payload",
                    ));
                }
                return Ok(lease);
            }
            return Err(IsolatedError::conflict(
                "lease already has an in-flight dispatch_id",
            ));
        }
        if lease.state != ComputerSurfaceLeaseState::Dispatching {
            self.prepare_dispatch(guest_id, lease_id, event.clone())
        } else {
            Err(IsolatedError::invalid_state(
                "dispatching lease is missing a bound dispatch record",
            ))
        }
    }

    pub fn terminate(
        &mut self,
        guest_id: &str,
        reason: IsolatedCleanupReason,
    ) -> IsolatedResult<IsolatedGuestRecord> {
        let mut guest = self.require_guest(guest_id)?;
        let now = self.clock.now();
        if guest.terminal.is_none() {
            match reason {
                IsolatedCleanupReason::Success => {
                    if guest.phase != IsolatedGuestPhase::Closing {
                        guest.transition(IsolatedGuestPhase::Closing, now)?;
                    }
                    guest.disposition = Some("success".into());
                }
                IsolatedCleanupReason::Cancel => {
                    guest.terminate(IsolatedGuestTerminal::Interrupted, now, "cancel")?;
                }
                IsolatedCleanupReason::Timeout
                | IsolatedCleanupReason::HelperFailure
                | IsolatedCleanupReason::GuestCrash => {
                    guest.terminate(IsolatedGuestTerminal::Failed, now, &format!("{reason:?}"))?;
                }
                IsolatedCleanupReason::HostCrash
                | IsolatedCleanupReason::Disconnect
                | IsolatedCleanupReason::Restart => {
                    guest.terminate(
                        IsolatedGuestTerminal::Interrupted,
                        now,
                        &format!("{reason:?}"),
                    )?;
                }
            }
        }
        for mut lease in self.store.list_leases()? {
            if lease.guest_id == guest.guest_id && !lease.state.is_terminal() {
                let next = if matches!(
                    lease.dispatch.as_ref().map(|dispatch| dispatch.state),
                    Some(ComputerDispatchState::Injected)
                ) {
                    ComputerSurfaceLeaseState::Uncertain
                } else {
                    ComputerSurfaceLeaseState::Revoked
                };
                if next == ComputerSurfaceLeaseState::Uncertain {
                    if let Some(dispatch) = lease.dispatch.as_mut() {
                        dispatch.state = ComputerDispatchState::Uncertain;
                        dispatch.completed_at = Some(now);
                        dispatch.error_code = Some(IsolatedErrorCode::UncertainOutcome);
                    }
                    lease.state = ComputerSurfaceLeaseState::Uncertain;
                    lease.revision = lease.revision.saturating_add(1);
                    lease.updated_at = now;
                } else {
                    lease.transition(next, now, Some("guest terminated")).ok();
                }
                self.store.save_lease(&lease)?;
            }
        }
        self.store.save_guest(&guest)?;
        Ok(guest)
    }

    /// Tear the guest down, then build a receipt by *re-observing* what is
    /// actually left.
    ///
    /// Teardown records every failure it hits rather than discarding it, and
    /// the receipt is produced by [`HostCleanupProbe`], which reads the
    /// filesystem, the occupancy store, and the guest handle again. A failed
    /// overlay deletion therefore surfaces as an unresolved receipt instead of
    /// disappearing with the in-memory key.
    pub fn cleanup(
        &mut self,
        guest_id: &str,
    ) -> IsolatedResult<(IsolatedGuestRecord, CleanupReceipt)> {
        let guest = self.require_guest(guest_id)?;
        if guest.phase != IsolatedGuestPhase::Closing {
            return Err(IsolatedError::invalid_state(
                "cleanup requires a closing guest",
            ));
        }
        let mut teardown_errors: Vec<String> = Vec::new();

        self.simulator.destroy(&guest.guest_id);

        if let Err(error) = remove_marker(&self.helper_marker_path(&guest.guest_id)) {
            teardown_errors.push(format!("helper marker: {}", error.message));
        }
        if let Err(error) = remove_marker(&self.overlay_path(&guest.guest_id)) {
            teardown_errors.push(format!("overlay: {}", error.message));
        }
        if let Err(error) = remove_marker(&self.channel_marker_path(&guest.surface.incarnation)) {
            teardown_errors.push(format!("channel: {}", error.message));
        }
        self.secrets.remove(&guest.surface.incarnation);
        if let Some(key) = self.occupancy_keys.remove(&guest.guest_id) {
            if let Err(error) = self.occupancy.release(&key, &guest.agent_id) {
                teardown_errors.push(format!("occupancy: {}", error.message));
                // Put the key back: the resource is still ours to account for.
                self.occupancy_keys.insert(guest.guest_id.clone(), key);
            }
        }

        let receipt = self.observe_cleanup(&guest, teardown_errors)?;
        if receipt.guest_id != guest.guest_id || receipt.surface != guest.surface {
            return Err(IsolatedError::unauthorized(
                "cleanup receipt does not match the guest incarnation",
            ));
        }

        let mut guest = self.require_guest(guest_id)?;
        guest.updated_at = self.clock.now();
        if receipt.is_exact() {
            guest.resident_frame_bytes = 0;
            guest.cleaned = true;
        } else {
            // An unresolved receipt must not read as a clean guest.
            guest.cleaned = false;
            guest.disposition = Some(format!(
                "cleanup unresolved: {}",
                receipt.unresolved.join("; ")
            ));
        }
        self.store.save_guest(&guest)?;
        Ok((guest, receipt))
    }

    /// Cleanup that insists on an exact receipt, surfacing anything unresolved
    /// as [`IsolatedErrorCode::UncertainOutcome`].
    pub fn cleanup_exact(&mut self, guest_id: &str) -> IsolatedResult<IsolatedGuestRecord> {
        let (guest, receipt) = self.cleanup(guest_id)?;
        receipt.require_exact()?;
        Ok(guest)
    }

    /// Re-derive post-teardown state for one guest.
    pub fn observe_cleanup(
        &self,
        guest: &IsolatedGuestRecord,
        teardown_errors: Vec<String>,
    ) -> IsolatedResult<CleanupReceipt> {
        let probe = HostCleanupProbe {
            host: self,
            teardown_errors,
        };
        CleanupReceipt::observe(&probe, &guest.guest_id, &guest.surface, self.clock.now())
    }

    pub fn project(&self, guest_id: &str) -> IsolatedResult<IsolatedVisualProjection> {
        let guest = self.require_guest(guest_id)?;
        let lease = self
            .store
            .list_leases()?
            .into_iter()
            .filter(|lease| lease.guest_id == guest.guest_id)
            .max_by_key(|lease| lease.queue_sequence);
        Ok(project_guest(&guest, lease.as_ref(), false))
    }

    pub fn guest(&self, guest_id: &str) -> IsolatedResult<IsolatedGuestRecord> {
        self.require_guest(guest_id)
    }

    pub fn leases(&self) -> IsolatedResult<Vec<ComputerSurfaceLease>> {
        self.store.list_leases()
    }

    pub fn fail_next_injector(&mut self) {
        self.simulator.fail_next_input();
    }

    pub fn occupancy(&self) -> &OccupancyStore {
        &self.occupancy
    }

    pub fn simulator(&self) -> &IsolatedSimulator {
        &self.simulator
    }

    pub fn reopen(self) -> IsolatedResult<Self> {
        let root = self.store.root().to_path_buf();
        let clock = Arc::clone(&self.clock);
        let resolver = HermeticResolver::new(self.resolver.store().clone());
        drop(self);
        IsolatedVisualHost::open(root, clock, resolver)
    }

    fn require_guest(&self, guest_id: &str) -> IsolatedResult<IsolatedGuestRecord> {
        validate_id("guest_id", guest_id)?;
        self.store
            .load_guest(guest_id)?
            .ok_or_else(|| IsolatedError::unauthorized("unknown isolated guest"))
    }

    fn require_live_guest(&self, guest_id: &str) -> IsolatedResult<IsolatedGuestRecord> {
        let guest = self.require_guest(guest_id)?;
        if !guest.is_live() {
            return Err(IsolatedError::invalid_state(
                "old guest incarnation is not resumable",
            ));
        }
        Ok(guest)
    }

    fn require_matching_lease(
        &self,
        guest: &IsolatedGuestRecord,
        lease_id: &str,
    ) -> IsolatedResult<ComputerSurfaceLease> {
        validate_id("lease_id", lease_id)?;
        let lease = self
            .store
            .load_lease(lease_id)?
            .ok_or_else(|| IsolatedError::unauthorized("unknown surface lease"))?;
        if lease.guest_id != guest.guest_id
            || lease.run_id != guest.run_id
            || lease.work_attempt_id != guest.work_attempt_id
            || lease.agent_id != guest.agent_id
            || lease.surface != guest.surface
            || lease.conflict_domain_id != guest.conflict_domain_id
        {
            return Err(IsolatedError::unauthorized(
                "lease identity does not match the live guest fence",
            ));
        }
        Ok(lease)
    }
}

/// Re-derives post-teardown state from the underlying resources.
///
/// It deliberately reads the filesystem and the occupancy store rather than
/// the host's in-memory bookkeeping. `occupancy_keys` is consulted only to
/// learn *which* key to re-read; the verdict comes from the store on disk.
struct HostCleanupProbe<'a> {
    host: &'a IsolatedVisualHost,
    /// Failures teardown already hit. Carried through so they cannot be lost
    /// even if the resource happens to look released afterwards.
    teardown_errors: Vec<String>,
}

impl std::fmt::Debug for HostCleanupProbe<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostCleanupProbe")
            .field("store_root", &self.host.store.root())
            .field("teardown_errors", &self.teardown_errors)
            .finish()
    }
}

impl HostCleanupProbe<'_> {
    /// A marker file that still exists means the resource was not released.
    /// An unreadable path is `Unknown`, never `Released`.
    fn marker_state(path: &Path) -> (ResourceState, Option<String>) {
        match fs::symlink_metadata(path) {
            Ok(_) => (
                ResourceState::Present,
                Some(format!("{} still exists", path.display())),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (ResourceState::Released, None)
            }
            Err(error) => (
                ResourceState::Unknown,
                Some(format!("{} is unreadable ({error})", path.display())),
            ),
        }
    }

    fn result(
        &self,
        resource: &str,
        method: &str,
        state: ResourceState,
        detail: Option<String>,
        guest_id: &str,
        incarnation: &str,
    ) -> ResourceProbeResult {
        // A teardown error for this resource downgrades the verdict even if the
        // resource now looks gone: we could not establish it cleanly.
        let matching: Vec<&String> = self
            .teardown_errors
            .iter()
            .filter(|error| error.starts_with(resource))
            .collect();
        let (state, detail) = if matching.is_empty() {
            (state, detail)
        } else {
            (
                ResourceState::Unknown,
                Some(format!("teardown reported: {}", matching[0])),
            )
        };
        ResourceProbeResult {
            resource: resource.to_string(),
            method: method.to_string(),
            state,
            detail,
            guest_id: guest_id.to_string(),
            surface_incarnation: incarnation.to_string(),
        }
    }
}

impl CleanupProbe for HostCleanupProbe<'_> {
    fn probe_id(&self) -> &'static str {
        "host_filesystem_occupancy_probe_v1"
    }

    fn probe(
        &self,
        guest_id: &str,
        surface: &ComputerSurfaceBinding,
    ) -> IsolatedResult<Vec<ResourceProbeResult>> {
        let incarnation = surface.incarnation.as_str();
        let mut results = Vec::new();

        let (state, detail) = Self::marker_state(&self.host.helper_marker_path(guest_id));
        results.push(self.result(
            "helper_process",
            "filesystem_marker",
            state,
            detail,
            guest_id,
            incarnation,
        ));

        // The guest handle itself is asked, not a record of what we intended.
        let vm_present = self.host.simulator.attached(guest_id);
        results.push(self.result(
            "guest_vm",
            "guest_handle",
            if vm_present {
                ResourceState::Present
            } else {
                ResourceState::Released
            },
            vm_present.then(|| "guest is still attached".to_string()),
            guest_id,
            incarnation,
        ));

        let (state, detail) = Self::marker_state(&self.host.overlay_path(guest_id));
        results.push(self.result(
            "overlay",
            "filesystem_symlink_metadata",
            state,
            detail,
            guest_id,
            incarnation,
        ));

        let (state, detail) = Self::marker_state(&self.host.channel_marker_path(incarnation));
        results.push(self.result(
            "channel",
            "filesystem_marker",
            state,
            detail,
            guest_id,
            incarnation,
        ));

        // Re-read the occupancy store, not the in-memory key map.
        let (state, detail) = match self.host.occupancy_keys.get(guest_id) {
            None => (ResourceState::Released, None),
            Some(key) => {
                if self.host.occupancy.holds(key) {
                    (
                        ResourceState::Present,
                        Some("occupancy record still present".to_string()),
                    )
                } else {
                    (ResourceState::Released, None)
                }
            }
        };
        results.push(self.result(
            "occupancy",
            "occupancy_store",
            state,
            detail,
            guest_id,
            incarnation,
        ));

        let resident = self.host.simulator.resident_bytes(guest_id);
        results.push(self.result(
            "resident_frames",
            "guest_handle",
            if resident == 0 {
                ResourceState::Released
            } else {
                ResourceState::Present
            },
            (resident != 0).then(|| format!("{resident} bytes resident")),
            guest_id,
            incarnation,
        ));

        Ok(results)
    }

    fn resident_bytes(&self, guest_id: &str) -> IsolatedResult<u64> {
        Ok(self.host.simulator.resident_bytes(guest_id))
    }
}

fn write_marker(path: &Path, bytes: &[u8]) -> IsolatedResult<()> {
    fs::write(path, bytes).map_err(|error| {
        IsolatedError::internal(format!("cannot write {} ({error})", path.display()))
    })
}

/// Remove a resource marker, reporting failure instead of discarding it.
fn remove_marker(path: &Path) -> IsolatedResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(IsolatedError::internal(format!(
            "cannot remove {} ({error})",
            path.display()
        ))),
    }
}

fn duration_exceeded(guest: &IsolatedGuestRecord, now: DateTime<Utc>) -> bool {
    guest.started_at.is_some_and(|started| {
        now >= started + Duration::seconds(guest.limits.duration_seconds as i64)
    })
}

fn fresh_secret(guest_id: &str, now: DateTime<Utc>) -> [u8; CHANNEL_SECRET_BYTES] {
    let digest = sha256_hex(
        format!(
            "{guest_id}:{}:{}",
            now.timestamp_nanos_opt().unwrap_or(0),
            Uuid::new_v4()
        )
        .as_bytes(),
    );
    let mut secret = [0u8; CHANNEL_SECRET_BYTES];
    for (index, chunk) in digest
        .as_bytes()
        .chunks(2)
        .enumerate()
        .take(CHANNEL_SECRET_BYTES)
    {
        secret[index] =
            u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("00"), 16).unwrap_or(0);
    }
    secret
}
