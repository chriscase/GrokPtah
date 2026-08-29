use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::cleanup::{
    ChannelRevocationObservation, HelperExitObservation, IsolatedCleanupEvidence,
    IsolatedCleanupObservation, IsolatedCleanupReason, OccupancyReleaseObservation,
    OverlayRemovalObservation, VmStopObservation,
};
use crate::clock::HostClock;
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
use crate::occupancy::{
    resource_key, OccupancyRecord, OccupancyState, OccupancyStore, PRIMARY_OVERLAY_ID,
    PRIMARY_SURFACE_ID,
};
use crate::preflight::IsolatedPreflight;
use crate::projection::{project_guest, IsolatedVisualProjection};
use crate::protocol::{
    mac_frame, verify_frame_mac, IsolatedFrameMeta, IsolatedInputEvent, ResidentFrame,
    CHANNEL_SECRET_BYTES,
};
use crate::resolver::{HermeticResolver, ResolvedSource};
use crate::simulator::IsolatedSimulator;
use crate::store::IsolatedVisualStore;

pub struct IsolatedVisualHost {
    store: IsolatedVisualStore,
    clock: Arc<dyn HostClock>,
    resolver: HermeticResolver,
    simulator: IsolatedSimulator,
    secrets: BTreeMap<String, [u8; CHANNEL_SECRET_BYTES]>,
    preflight: IsolatedPreflight,
    next_queue_sequence: u64,
    occupancy: OccupancyStore,
    artifact_root: Option<PathBuf>,
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
        let now = clock.now();
        let store = IsolatedVisualStore::open(root, now)?;
        let occupancy = OccupancyStore::open(store.root().join("occupancy"))?;
        let mut host = Self {
            store,
            clock,
            resolver,
            simulator: IsolatedSimulator::new(),
            secrets: BTreeMap::new(),
            preflight: IsolatedPreflight::inspect(artifact_root)?,
            next_queue_sequence: 1,
            occupancy,
            artifact_root: artifact_root.map(Path::to_path_buf),
        };
        host.next_queue_sequence = host
            .store
            .list_leases()?
            .into_iter()
            .map(|lease| lease.queue_sequence)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        Ok(host)
    }

    pub fn preflight(&self) -> &IsolatedPreflight {
        &self.preflight
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

    /// Simulator guests may be created without packaged admission. Evidence
    /// class remains ineligible for VM qualification.
    pub fn create_guest(
        &mut self,
        request: CreateGuestRequest,
    ) -> IsolatedResult<IsolatedGuestRecord> {
        self.create_guest_with_class(request, IsolatedEvidenceClass::SimulatorIneligible)
    }

    /// Packaged / Virtualization.framework guests require admitted helper and
    /// image identities. Evidence class stays ineligible until an
    /// authoritative launch receipt is observed.
    pub fn create_packaged_guest(
        &mut self,
        request: CreateGuestRequest,
    ) -> IsolatedResult<IsolatedGuestRecord> {
        self.preflight.fail_closed_launch()?;
        if !self.preflight.helper_admitted || !self.preflight.image_admitted {
            return Err(IsolatedError::unauthorized(
                "packaged guest create requires admitted helper and image identities",
            ));
        }
        self.create_guest_with_class(request, IsolatedEvidenceClass::SimulatorIneligible)
    }

    fn create_guest_with_class(
        &mut self,
        request: CreateGuestRequest,
        evidence_class: IsolatedEvidenceClass,
    ) -> IsolatedResult<IsolatedGuestRecord> {
        if evidence_class == IsolatedEvidenceClass::VirtualizationFramework {
            return Err(IsolatedError::unauthorized(
                "create cannot mint Virtualization.framework evidence without a launch receipt",
            ));
        }
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
        let image_digest = request
            .source
            .guest_image_sha256
            .clone()
            .unwrap_or_else(|| request.helper.content_sha256.clone());
        let occupancy_resource_key =
            resource_key(&image_digest, PRIMARY_OVERLAY_ID, PRIMARY_SURFACE_ID);
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
            occupancy_resource_key: occupancy_resource_key.clone(),
            evidence_class,
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
        self.occupancy.try_acquire(OccupancyRecord {
            schema_version: SCHEMA_VERSION,
            resource_key: occupancy_resource_key,
            owner_id: guest.agent_id.clone(),
            guest_id: guest.guest_id.clone(),
            surface_incarnation: guest.surface.incarnation.clone(),
            image_digest,
            overlay_id: PRIMARY_OVERLAY_ID.to_string(),
            vm_instance_id: None,
            state: OccupancyState::Clear,
            updated_at: now,
        })?;
        self.write_guest_presence(&guest)?;
        self.store.save_guest(&guest)?;
        self.simulator.attach(&guest.guest_id);
        self.secrets.insert(
            guest.surface.incarnation.clone(),
            fresh_secret(&guest.guest_id, now),
        );
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
        self.store.save_lease(&lease)?;
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
        self.store.save_lease(&lease)?;
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

    pub fn observe_cleanup(&self, guest_id: &str) -> IsolatedResult<IsolatedCleanupObservation> {
        let guest = self.require_guest(guest_id)?;
        let occupancy_held = self.occupancy_held(&guest).unwrap_or(true);
        Ok(IsolatedCleanupObservation {
            guest_id: guest.guest_id.clone(),
            surface: guest.surface.clone(),
            helper_exit: Some(HelperExitObservation {
                guest_id: guest.guest_id.clone(),
                surface_incarnation: guest.surface.incarnation.clone(),
                helper_alive: self.helper_alive_path(&guest.guest_id).exists(),
                audit_identity: guest.helper.helper_id.clone(),
            }),
            vm_stop: Some(VmStopObservation {
                guest_id: guest.guest_id.clone(),
                surface_incarnation: guest.surface.incarnation.clone(),
                vm_present: self.vm_present_path(&guest.guest_id).exists(),
            }),
            overlay_removed: Some(OverlayRemovalObservation {
                guest_id: guest.guest_id.clone(),
                surface_incarnation: guest.surface.incarnation.clone(),
                overlay_present: self.overlay_path(&guest.guest_id).exists(),
            }),
            channel_revoked: Some(ChannelRevocationObservation {
                guest_id: guest.guest_id.clone(),
                surface_incarnation: guest.surface.incarnation.clone(),
                channel_present: self.channel_path(&guest.guest_id).exists(),
            }),
            occupancy_released: Some(OccupancyReleaseObservation {
                guest_id: guest.guest_id.clone(),
                surface_incarnation: guest.surface.incarnation.clone(),
                occupancy_held,
            }),
            resident_bytes: Some(self.simulator.resident_bytes(&guest.guest_id)),
        })
    }

    pub fn cleanup(&mut self, guest_id: &str) -> IsolatedResult<IsolatedGuestRecord> {
        let guest = self.require_guest(guest_id)?;
        if guest.phase != IsolatedGuestPhase::Closing {
            return Err(IsolatedError::invalid_state(
                "cleanup requires a closing guest",
            ));
        }
        self.simulator.destroy(&guest.guest_id);
        let _ = fs::remove_file(self.helper_alive_path(&guest.guest_id));
        let _ = fs::remove_file(self.overlay_path(&guest.guest_id));
        let _ = fs::remove_file(self.channel_path(&guest.guest_id));
        let _ = fs::remove_file(self.vm_present_path(&guest.guest_id));
        self.secrets.remove(&guest.surface.incarnation);
        if !guest.occupancy_resource_key.is_empty() {
            self.occupancy
                .release(&guest.occupancy_resource_key, &guest.agent_id)?;
        } else if let Some(record) = self.occupancy.find_for_guest(&guest.guest_id)? {
            self.occupancy
                .release(&record.resource_key, &guest.agent_id)?;
        }
        let evidence = IsolatedCleanupEvidence::from_observations(
            self.observe_cleanup(guest_id)?,
            self.clock.now(),
        )?;
        if evidence.guest_id != guest.guest_id || evidence.surface != guest.surface {
            return Err(IsolatedError::unauthorized(
                "cleanup evidence does not match the guest incarnation",
            ));
        }
        let mut guest = self.require_guest(guest_id)?;
        guest.resident_frame_bytes = 0;
        guest.cleaned = true;
        guest.updated_at = self.clock.now();
        self.store.save_guest(&guest)?;
        Ok(guest)
    }

    pub fn cleanup_from_observations(
        &mut self,
        guest_id: &str,
        observation: IsolatedCleanupObservation,
    ) -> IsolatedResult<IsolatedGuestRecord> {
        let evidence = IsolatedCleanupEvidence::from_observations(observation, self.clock.now())?;
        let guest = self.require_guest(guest_id)?;
        if evidence.guest_id != guest.guest_id || evidence.surface != guest.surface {
            return Err(IsolatedError::unauthorized(
                "cleanup evidence does not match the guest incarnation",
            ));
        }
        self.cleanup(guest_id)
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
        let artifact_root = self.artifact_root.clone();
        drop(self);
        IsolatedVisualHost::open_with_artifacts(root, clock, resolver, artifact_root.as_deref())
    }

    fn overlay_path(&self, guest_id: &str) -> PathBuf {
        self.store
            .root()
            .join("overlays")
            .join(format!("{guest_id}.overlay"))
    }

    fn helper_alive_path(&self, guest_id: &str) -> PathBuf {
        self.store
            .root()
            .join("helpers")
            .join(format!("{guest_id}.alive"))
    }

    fn vm_present_path(&self, guest_id: &str) -> PathBuf {
        self.store
            .root()
            .join("vms")
            .join(format!("{guest_id}.present"))
    }

    fn channel_path(&self, guest_id: &str) -> PathBuf {
        self.store
            .root()
            .join("channels")
            .join(format!("{guest_id}.channel"))
    }

    fn write_guest_presence(&self, guest: &IsolatedGuestRecord) -> IsolatedResult<()> {
        for dir in ["overlays", "helpers", "vms", "channels"] {
            fs::create_dir_all(self.store.root().join(dir))
                .map_err(|error| IsolatedError::internal(error.to_string()))?;
        }
        fs::write(
            self.overlay_path(&guest.guest_id),
            guest.guest_id.as_bytes(),
        )
        .map_err(|error| IsolatedError::internal(error.to_string()))?;
        fs::write(self.helper_alive_path(&guest.guest_id), b"alive")
            .map_err(|error| IsolatedError::internal(error.to_string()))?;
        fs::write(self.vm_present_path(&guest.guest_id), b"present")
            .map_err(|error| IsolatedError::internal(error.to_string()))?;
        fs::write(self.channel_path(&guest.guest_id), b"channel")
            .map_err(|error| IsolatedError::internal(error.to_string()))?;
        Ok(())
    }

    fn occupancy_held(&self, guest: &IsolatedGuestRecord) -> IsolatedResult<bool> {
        if !guest.occupancy_resource_key.is_empty() {
            return Ok(self
                .occupancy
                .inspect(&guest.occupancy_resource_key)?
                .is_held());
        }
        Ok(self
            .occupancy
            .find_for_guest(&guest.guest_id)?
            .is_some_and(|record| record.state.is_held()))
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
