use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::coordination::{ComputerDispatchClaim, ComputerSurfaceLease, ComputerSurfaceLeaseState};
use super::policy::ComputerPolicy;
use super::projection::{
    not_available, project_events, project_run_at, ComputerRunCapacity, ComputerRunEventPage,
    ComputerRunProjection, ComputerSurfaceCoordination, ComputerSurfaceCoordinationState,
    ComputerSurfaceOccupant, ComputerUncertainSurfaceLease,
};
use super::store::{ComputerStore, MutationClaim, MutationStamp};
use super::types::{
    validate_id, ActionClass, ActionGrant, ActionOutcome, ComputerAction, ComputerAttentionPoint,
    ComputerAuthorityToken, ComputerBackend, ComputerBackendAttestation,
    ComputerControlDisposition, ComputerEmergencyControlToken, ComputerError, ComputerErrorCode,
    ComputerObservation, ComputerResult, ComputerRun, ComputerRunState, ComputerSurfaceEvent,
    ComputerTarget, ComputerUseLimits, ObservationAuthority, ResolvedAgentComputerRunAdmission,
};

pub struct ComputerUseService {
    backend: Arc<dyn ComputerBackend>,
    store: ComputerStore,
    policy: ComputerPolicy,
    backend_attestation: ComputerBackendAttestation,
    agent_work_store: parking_lot::Mutex<Option<crate::orchestration::OrchStore>>,
    #[cfg(test)]
    trust_unbound_agent_work_for_tests: std::sync::atomic::AtomicBool,
}

impl ComputerUseService {
    /// Construct an embedder-provided backend. Public backend claims remain
    /// unproven until GrokPtah binds the exact built-in implementation.
    pub fn new(backend: Arc<dyn ComputerBackend>, store: ComputerStore) -> Self {
        Self::new_trusted(backend, store, ComputerBackendAttestation::unproven())
    }

    /// Construct the built-in simulator. The attestation is bound to the same
    /// simulator instance retained by this service and cannot be transferred
    /// to an arbitrary downstream backend.
    pub fn new_simulator(
        backend: Arc<super::simulator::SimulatorBackend>,
        store: ComputerStore,
    ) -> Self {
        let backend_attestation = backend.host_attestation();
        Self::new_trusted(backend, store, backend_attestation)
    }

    pub(crate) fn new_trusted(
        backend: Arc<dyn ComputerBackend>,
        store: ComputerStore,
        backend_attestation: ComputerBackendAttestation,
    ) -> Self {
        Self {
            backend,
            store,
            policy: ComputerPolicy,
            backend_attestation,
            agent_work_store: parking_lot::Mutex::new(None),
            #[cfg(test)]
            trust_unbound_agent_work_for_tests: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn bind_agent_work_store(
        &self,
        store: crate::orchestration::OrchStore,
    ) -> ComputerResult<()> {
        let mut current = self.agent_work_store.lock();
        if let Some(bound) = current.as_ref() {
            if bound.root() != store.root() {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "Computer Use service is already bound to another Work ledger",
                ));
            }
            return Ok(());
        }
        *current = Some(store);
        Ok(())
    }

    fn with_active_agent_work<T>(
        &self,
        run: &ComputerRun,
        operation: impl FnOnce() -> ComputerResult<T>,
    ) -> ComputerResult<T> {
        let Some(binding) = run.work_attempt.as_ref() else {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "Agent Computer Run is missing its host-frozen WorkAttempt binding",
            ));
        };
        self.with_active_agent_binding(
            binding,
            run.owner_session_id,
            run.workspace.as_deref().unwrap_or_default(),
            operation,
        )
    }

    fn with_active_agent_binding<T>(
        &self,
        binding: &super::types::ComputerWorkAttemptBinding,
        owner_session_id: Uuid,
        workspace: &str,
        operation: impl FnOnce() -> ComputerResult<T>,
    ) -> ComputerResult<T> {
        binding.validate()?;
        #[cfg(test)]
        if self
            .trust_unbound_agent_work_for_tests
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return operation();
        }
        let store = self.agent_work_store.lock().clone().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "Agent Computer Use is not bound to the host Work ledger",
            )
        })?;
        store
            .with_active_computer_work_attempt(
                &binding.work_id,
                &binding.work_attempt_id,
                (&binding.agent_id, binding.agent_spec_revision),
                (owner_session_id, workspace),
                operation,
            )
            .map_err(|_| {
                ComputerError::new(
                    ComputerErrorCode::PermissionRevoked,
                    "Agent Computer Use WorkAttempt authority is no longer active",
                )
            })?
    }

    #[cfg(test)]
    fn trust_unbound_agent_work_for_tests(&self) {
        self.trust_unbound_agent_work_for_tests
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn capabilities(&self) -> super::types::ComputerCapabilities {
        self.backend_attestation
            .attest_capabilities(self.backend.capabilities())
            .unwrap_or_else(|_| super::types::ComputerCapabilities::unproven("unproven"))
    }

    pub fn list_runs(&self) -> ComputerResult<Vec<ComputerRun>> {
        self.store.list_runs()
    }

    pub fn get_run(&self, run_id: &str) -> ComputerResult<Option<ComputerRun>> {
        self.store.load_run(run_id)
    }

    /// Return the exact opaque handles needed for an owning local operator to
    /// reconcile this run's uncertain physical dispatch. No outcome, target
    /// content, evidence token, or backend text is exposed.
    pub fn uncertain_surface_lease(
        &self,
        run_id: &str,
    ) -> ComputerResult<Option<ComputerUncertainSurfaceLease>> {
        validate_id("run_id", run_id)?;
        let _run = self.store.load_run(run_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown computer run")
        })?;
        Ok(self
            .store
            .list_surface_leases()?
            .into_iter()
            .find(|lease| {
                lease.run_id == run_id
                    && lease.state == super::coordination::ComputerSurfaceLeaseState::Uncertain
                    && lease.dispatch.as_ref().is_some_and(|dispatch| {
                        dispatch.state == super::coordination::ComputerDispatchState::Uncertain
                    })
            })
            .map(|lease| ComputerUncertainSurfaceLease {
                lease_id: lease.lease_id,
                expected_revision: lease.revision,
                surface_id: lease.surface.surface_id,
                incarnation: lease.surface.incarnation,
            }))
    }

    /// Explain one session-owned Agent Run's current position on its physical
    /// input conflict domain without exposing any lease or authority handle.
    ///
    /// This is intentionally a local-operator projection. Serving it through
    /// a workspace-scoped coordinator would reveal host-wide queue depth and
    /// another workspace's active Agent identity.
    pub fn local_surface_coordination(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<Option<ComputerSurfaceCoordination>> {
        let _run = self.load_owned_run(owner_session_id, run_id)?;
        let leases = self.store.list_surface_leases()?;
        let mut current = leases
            .iter()
            .filter(|lease| {
                lease.run_id == run_id
                    && (lease.state == ComputerSurfaceLeaseState::Uncertain
                        || (!lease.state.is_terminal() && lease.expires_at > now))
            })
            .collect::<Vec<_>>();
        if current.len() > 1 {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "Agent Computer Run owns multiple live surface coordination records",
            ));
        }
        let Some(lease) = current.pop() else {
            return Ok(None);
        };
        let domain = leases
            .iter()
            .filter(|candidate| {
                candidate.conflict_domain_id == lease.conflict_domain_id
                    && (candidate.state == ComputerSurfaceLeaseState::Uncertain
                        || (!candidate.state.is_terminal() && candidate.expires_at > now))
            })
            .collect::<Vec<_>>();
        let owners = domain
            .iter()
            .copied()
            .filter(|candidate| candidate.state.owns_domain_capacity())
            .collect::<Vec<_>>();
        if owners.len() > 1 {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "Computer surface conflict domain has multiple capacity owners",
            ));
        }
        let newest_sequence = domain
            .iter()
            .map(|candidate| candidate.queue_sequence)
            .max()
            .unwrap_or(0);
        let mut waiters = domain
            .iter()
            .copied()
            .filter(|candidate| candidate.state == ComputerSurfaceLeaseState::Queued)
            .collect::<Vec<_>>();
        waiters.sort_by(|left, right| {
            right
                .effective_priority(newest_sequence)
                .cmp(&left.effective_priority(newest_sequence))
                .then_with(|| left.queue_sequence.cmp(&right.queue_sequence))
                .then_with(|| left.lease_id.cmp(&right.lease_id))
        });
        let queue_depth = u32::try_from(waiters.len()).map_err(|_| {
            ComputerError::new(
                ComputerErrorCode::Internal,
                "Computer surface queue depth exceeds its public projection bound",
            )
        })?;
        let queue_position = waiters
            .iter()
            .position(|candidate| candidate.lease_id == lease.lease_id)
            .map(|position| u32::try_from(position + 1))
            .transpose()
            .map_err(|_| {
                ComputerError::new(
                    ComputerErrorCode::Internal,
                    "Computer surface queue position exceeds its public projection bound",
                )
            })?;
        let state = match lease.state {
            ComputerSurfaceLeaseState::Queued => ComputerSurfaceCoordinationState::Queued,
            ComputerSurfaceLeaseState::Granted => ComputerSurfaceCoordinationState::Granted,
            ComputerSurfaceLeaseState::Dispatching => ComputerSurfaceCoordinationState::Dispatching,
            ComputerSurfaceLeaseState::Uncertain => ComputerSurfaceCoordinationState::Uncertain,
            _ => {
                return Err(ComputerError::new(
                    ComputerErrorCode::Internal,
                    "Computer surface coordination record has an invalid projected state",
                ))
            }
        };
        let active = owners.first().map(|owner| ComputerSurfaceOccupant {
            agent_id: owner.agent_id.clone(),
            work_id: owner.work_id.clone(),
            run_id: owner.run_id.clone(),
        });
        Ok(Some(ComputerSurfaceCoordination {
            state,
            queue_position,
            queue_depth,
            owns_surface: owners
                .first()
                .is_some_and(|owner| owner.lease_id == lease.lease_id),
            blocked_by_uncertain_outcome: domain
                .iter()
                .any(|candidate| candidate.state == ComputerSurfaceLeaseState::Uncertain),
            active,
            expires_at: lease.expires_at,
            updated_at: lease.updated_at,
        }))
    }

    /// Local-operator projection of every run owned by one session, newest
    /// first. The desktop cockpit uses this gate, including unbound runs.
    /// Coordinator surfaces must not call this: they take
    /// [`super::reads::ComputerReadBinding`] on [`super::reads::ComputerRunReads`]
    /// so workspace binding is the authorization identity.
    pub fn list_session_run_projections(
        &self,
        owner_session_id: Uuid,
        now: DateTime<Utc>,
    ) -> ComputerResult<Vec<ComputerRunProjection>> {
        Ok(self
            .store
            .list_runs()?
            .iter()
            .filter(|run| run.owner_session_id == owner_session_id)
            .map(|run| project_run_at(run, now))
            .collect())
    }

    /// Local-operator projection of one session-owned run.
    ///
    /// Fails closed: an unknown run and a run owned by another session return
    /// the identical error, so a caller cannot use this to probe whether a run
    /// id exists outside its own session. Coordinator surfaces must use
    /// [`super::reads::ComputerRunReads`] with a [`super::reads::ComputerReadBinding`].
    pub fn project_session_run(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerRunProjection> {
        self.load_owned_run(owner_session_id, run_id)
            .map(|run| project_run_at(&run, now))
    }

    /// One bounded page of a session-owned run's durable event journal.
    ///
    /// A cursor older than the retained window is reported as expired rather
    /// than silently resuming mid-journal. Coordinator surfaces must use
    /// [`super::reads::ComputerRunReads`].
    pub fn session_run_events(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        after_seq: Option<u64>,
        limit: usize,
    ) -> ComputerResult<ComputerRunEventPage> {
        self.load_owned_run(owner_session_id, run_id)
            .map(|run| project_events(&run, after_seq, limit))
    }

    /// Local-operator ledger occupancy. Host-wide figures stay on this type
    /// and must not be served after a workspace gate.
    pub fn session_capacity(&self, owner_session_id: Uuid) -> ComputerResult<ComputerRunCapacity> {
        let runs = self.store.list_runs()?;
        let session_runs = runs
            .iter()
            .filter(|run| run.owner_session_id == owner_session_id);
        Ok(ComputerRunCapacity {
            max_run_records: ComputerStore::MAX_RUN_RECORDS as u32,
            stored_runs: runs.len() as u32,
            active_runs: runs.iter().filter(|run| !run.state.is_terminal()).count() as u32,
            session_runs: session_runs.clone().count() as u32,
            session_active_runs: session_runs.filter(|run| !run.state.is_terminal()).count() as u32,
        })
    }

    /// Single ownership gate for every scoped read.
    fn load_owned_run(&self, owner_session_id: Uuid, run_id: &str) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id).map_err(|_| not_available())?;
        self.store
            .load_run(run_id)?
            .filter(|run| run.owner_session_id == owner_session_id)
            .ok_or_else(not_available)
    }

    fn require_caller(
        &self,
        run: &ComputerRun,
        caller: &ComputerAuthorityToken,
    ) -> ComputerResult<()> {
        self.policy.authorize_caller(run, caller.principal())
    }

    pub async fn read_current_evidence(
        &self,
        caller: &ComputerAuthorityToken,
        run_id: &str,
        asset_id: &str,
    ) -> ComputerResult<Vec<u8>> {
        validate_id("run_id", run_id)?;
        validate_id("asset_id", asset_id)?;
        let run = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        self.policy.authorize_evidence(&run, caller.principal())?;
        let evidence = run
            .current_observation
            .as_ref()
            .and_then(|observation| observation.screenshot.as_ref())
            .filter(|evidence| evidence.asset_id == asset_id)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "evidence is not attached to the current observation",
                )
            })?;
        let bytes = self
            .backend
            .read_evidence(run_id, asset_id)
            .await?
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::BackendUnavailable,
                    "current observation evidence is unavailable",
                )
            })?;
        if bytes.len() as u64 != evidence.byte_len
            || format!("{:x}", Sha256::digest(&bytes)) != evidence.content_sha256
        {
            return Err(ComputerError::new(
                ComputerErrorCode::BackendFailure,
                "computer-use evidence failed integrity validation",
            ));
        }
        Ok(bytes)
    }

    pub fn create_run(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        workspace: Option<String>,
        target: ComputerTarget,
        limits: ComputerUseLimits,
    ) -> ComputerResult<ComputerRun> {
        target.validate()?;
        limits.validate()?;
        caller.principal().validate()?;
        let owner_session_id = caller.principal().session_id().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "create_run requires a host-issued local operator session",
            )
        })?;
        let payload = json!({
            "ownerSessionId": owner_session_id,
            "workspace": workspace.as_deref(),
            "target": target,
            "limits": limits,
        });
        if let Some(replayed) =
            self.begin_mutation(request_id, "create_run", &payload, caller, None)?
        {
            return replayed;
        }
        let result = (|| {
            self.store.can_create_run()?;
            let capabilities = self
                .backend_attestation
                .attest_capabilities(self.backend.capabilities())?;
            let invalid_native_proof = match capabilities.proof.backend_id() {
                crate::computer_use::MACOS_NATIVE_BACKEND_ID
                | crate::computer_use::MACOS_INTERRUPTED_BACKEND_ID => !matches!(
                    capabilities.proof,
                    crate::computer_use::ComputerCapabilityProof::ForegroundSemantic { .. }
                ),
                crate::computer_use::MACOS_BACKGROUND_SAFE_BACKEND_ID => !matches!(
                    capabilities.proof,
                    crate::computer_use::ComputerCapabilityProof::MeasuredBackgroundSafeSemantic { .. }
                ),
                _ => false,
            };
            if invalid_native_proof {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "native macOS Computer Use proof does not match its compiled-in execution mode",
                ));
            }
            let interned = self
                .store
                .intern_physical_domain(self.backend_attestation.physical_domain())?;
            let proof = interned.stamp_proof(capabilities.proof)?;
            proof.validate()?;
            let mut run = ComputerRun::new_with_isolation(
                owner_session_id,
                workspace,
                target,
                limits,
                caller.principal().clone(),
                interned.binding,
                proof,
            )?;
            run.record_audit("create_run", "accepted", None, None, None);
            self.store.save_run(&run)?;
            Ok(run)
        })();
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Host-only creation path for a durable Agent. The caller token can only
    /// be minted after `AgentHost` resolves the exact current AgentRecord/spec
    /// revision. Work/Attempt binding is added by the surface lease queue.
    pub(crate) fn create_agent_run(
        &self,
        caller: &ComputerAuthorityToken,
        admission: ResolvedAgentComputerRunAdmission,
    ) -> ComputerResult<ComputerRun> {
        admission.target.validate()?;
        admission.limits.validate()?;
        admission.binding.validate()?;
        caller.principal().validate()?;
        if caller.principal().agent_id().is_none()
            || caller.principal().agent_spec_revision().is_none()
        {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "create_agent_run requires a host-issued durable Agent principal",
            ));
        }
        let payload = json!({
            "ownerSessionId": admission.owner_session_id,
            "workId": admission.binding.work_id,
            "workAttemptId": admission.binding.work_attempt_id,
            "workspace": admission.workspace,
            "target": admission.target,
            "limits": admission.limits,
            "agentId": caller.principal().agent_id(),
            "agentSpecRevision": caller.principal().agent_spec_revision(),
        });
        self.with_active_agent_binding(
            &admission.binding,
            admission.owner_session_id,
            &admission.workspace,
            || Ok(()),
        )?;
        if let Some(replayed) = self.begin_mutation(
            &admission.request_id,
            "create_agent_run",
            &payload,
            caller,
            None,
        )? {
            return replayed;
        }
        let result = (|| {
            self.store.can_create_run()?;
            let capabilities = self
                .backend_attestation
                .attest_capabilities(self.backend.capabilities())?;
            let interned = self
                .store
                .intern_physical_domain(self.backend_attestation.physical_domain())?;
            let proof = interned.stamp_proof(capabilities.proof)?;
            proof.validate()?;
            let mut run = ComputerRun::new_with_isolation(
                admission.owner_session_id,
                Some(admission.workspace.clone()),
                admission.target.clone(),
                admission.limits,
                caller.principal().clone(),
                interned.binding,
                proof,
            )?;
            run.work_attempt = Some(admission.binding.clone());
            run.record_audit("create_agent_run", "accepted", None, None, None);
            self.with_active_agent_work(&run, || {
                self.store.save_run(&run)?;
                Ok(run.clone())
            })
        })();
        self.finish_mutation(&admission.request_id, &result)?;
        result
    }

    pub fn authorize(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
        expected_version: u64,
        grant: ActionGrant,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        grant.validate()?;
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({
            "runId": run_id,
            "expectedVersion": expected_version,
            "grant": grant,
        });
        if let Some(replayed) =
            self.begin_mutation(request_id, "authorize", &payload, caller, Some(&current))?
        {
            return replayed;
        }
        let authorize = || {
            self.store
                .update_run(run_id, |run| {
                    ensure_version(run, expected_version)?;
                    if run.control_disposition == ComputerControlDisposition::OperatorTakeover {
                        return Err(ComputerError::new(
                            ComputerErrorCode::InvalidState,
                            "operator takeover is absorbing; create a new computer run",
                        ));
                    }
                    self.policy
                        .authorize_grant(run, &grant, Utc::now(), caller.principal())?;
                    run.grant = Some(grant.clone());
                    run.last_error = None;
                    run.transition(ComputerRunState::Ready)?;
                    run.set_control_disposition(ComputerControlDisposition::AgentOwned);
                    run.record_audit("authorize", "granted", None, None, None);
                    Ok(())
                })
                .and_then(|run| run.ok_or_else(unknown_run))
        };
        let result = if current
            .initiating_principal
            .as_ref()
            .is_some_and(|principal| principal.agent_id().is_some())
        {
            self.with_active_agent_work(&current, authorize)
        } else {
            authorize()
        };
        if let Err(error) = &result {
            self.record_denial(run_id, "authorize", None, error);
        }
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Persist the redaction-safe UI evidence for one model-proposed action.
    /// This does not grant authority, dispatch input, persist model text, or
    /// alter the Run version used by the subsequent one-use approval. The
    /// trusted desktop calls it only after `AgentHost` returns a qualified,
    /// exact-observation proposal and after independently validating the
    /// semantic action against the local observation.
    pub fn record_agent_action_proposal(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action_class: ActionClass,
        attention: Option<ComputerAttentionPoint>,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        validate_id("observation_id", observation_id)?;
        if !matches!(action_class, ActionClass::Semantic | ActionClass::TextEntry) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "desktop agent proposals are limited to semantic actions",
            ));
        }
        if let Some(point) = attention {
            point.validate()?;
        }
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({
            "runId": run_id,
            "expectedVersion": expected_version,
            "observationId": observation_id,
            "actionClass": action_class,
            "attention": attention,
        });
        if let Some(replayed) = self.begin_mutation(
            request_id,
            "record_agent_action_proposal",
            &payload,
            caller,
            Some(&current),
        )? {
            return replayed;
        }
        let result = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                self.require_caller(run, caller)?;
                if run.state != ComputerRunState::Ready
                    || run
                        .current_observation
                        .as_ref()
                        .map(|observation| observation.observation_id.as_str())
                        != Some(observation_id)
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::StaleObservation,
                        "agent proposal is not bound to the current ready observation",
                    ));
                }
                run.updated_at = Utc::now();
                run.record_surface_audit(
                    ComputerSurfaceEvent::ActionProposed,
                    "action_proposed",
                    "staged",
                    Some(action_class),
                    Some(observation_id.into()),
                    None,
                    None,
                );
                if let Some(point) = attention {
                    run.record_surface_audit(
                        ComputerSurfaceEvent::AttentionMoved,
                        "attention",
                        "moved",
                        Some(action_class),
                        Some(observation_id.into()),
                        None,
                        Some(point),
                    );
                }
                run.record_surface_audit(
                    ComputerSurfaceEvent::ApprovalRequired,
                    "approval",
                    "required",
                    Some(action_class),
                    Some(observation_id.into()),
                    None,
                    None,
                );
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        if let Err(error) = &result {
            self.record_denial(
                run_id,
                "record_agent_action_proposal",
                Some(action_class),
                error,
            );
        }
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Record an explicit rejection of the exact still-current model proposal.
    /// Rejection carries no attention point so a replay consumer cannot mistake
    /// an old marker for current agent intent.
    pub fn record_agent_approval_rejected(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action_class: ActionClass,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        validate_id("observation_id", observation_id)?;
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({
            "runId": run_id,
            "expectedVersion": expected_version,
            "observationId": observation_id,
            "actionClass": action_class,
        });
        if let Some(replayed) = self.begin_mutation(
            request_id,
            "record_agent_approval_rejected",
            &payload,
            caller,
            Some(&current),
        )? {
            return replayed;
        }
        let result = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                self.require_caller(run, caller)?;
                if run.state != ComputerRunState::Ready
                    || run
                        .current_observation
                        .as_ref()
                        .map(|observation| observation.observation_id.as_str())
                        != Some(observation_id)
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::StaleObservation,
                        "approval rejection is not bound to the current ready observation",
                    ));
                }
                run.updated_at = Utc::now();
                run.record_surface_audit(
                    ComputerSurfaceEvent::ApprovalRejected,
                    "approval",
                    "rejected",
                    Some(action_class),
                    Some(observation_id.into()),
                    None,
                    None,
                );
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        if let Err(error) = &result {
            self.record_denial(
                run_id,
                "record_agent_approval_rejected",
                Some(action_class),
                error,
            );
        }
        self.finish_mutation(request_id, &result)?;
        result
    }

    pub async fn observe(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerObservation> {
        validate_id("run_id", run_id)?;
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({ "runId": run_id, "expectedVersion": expected_version });
        if let Some(replayed) =
            self.begin_mutation(request_id, "observe", &payload, caller, Some(&current))?
        {
            return replayed;
        }

        let mut observation_lease =
            match self.preflight_agent_surface_observation(&current, caller, expected_version) {
                Ok(lease) => lease,
                Err(error) => {
                    if error.code != ComputerErrorCode::Pending
                        && current
                            .initiating_principal
                            .as_ref()
                            .is_some_and(|principal| principal.agent_id().is_some())
                    {
                        if let Err(cleanup_error) = self
                            .revoke_active_observation_lease(run_id, "observation_preflight_failed")
                        {
                            return self.finish_and_return(request_id, Err(cleanup_error));
                        }
                    }
                    return self.finish_and_return(request_id, Err(error));
                }
            };

        let mut budget_error = None;
        let prepared = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                let now = Utc::now();
                if self.policy.run_limit_reached(run, now) {
                    let error = run_limit_error();
                    run.last_error = Some(error.clone());
                    run.transition(ComputerRunState::LimitReached)?;
                    revoke_authority(run);
                    run.record_audit("observe", "limit_reached", None, None, Some(error.code));
                    budget_error = Some(error);
                    return Ok(());
                }
                self.policy
                    .authorize_observation(run, now, caller.principal())?;
                run.transition(ComputerRunState::Observing)?;
                run.record_audit("observe", "started", None, None, None);
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));

        let result = match (prepared, budget_error) {
            (Ok(_), Some(error)) => {
                if let Some(lease) = observation_lease.take() {
                    if let Err(cleanup_error) =
                        self.revoke_observation_lease(&lease, "run_limit_reached")
                    {
                        return self.finish_and_return(request_id, Err(cleanup_error));
                    }
                }
                Err(error)
            }
            (Ok(prepared), None) => {
                // Observation identities cross the GUI/MCP projection boundary,
                // so they are minted by the host before capture. A backend may
                // use the ID to bind its element handles, but may not choose an
                // identifier that could carry observed document content.
                let observation_id = format!("observation-{}", Uuid::new_v4());
                let observed = self
                    .backend
                    .observe(run_id, &observation_id, &prepared.target, &prepared.limits)
                    .await;
                match observed {
                    Ok(observation) => {
                        let validated = if observation.observation_id != observation_id {
                            Err(ComputerError::new(
                                ComputerErrorCode::BackendFailure,
                                "backend returned an observation identity it did not receive",
                            ))
                        } else {
                            observation.validate(&prepared.limits)
                        }
                        .and_then(|()| self.policy.authorize_observation_exposure(&observation))
                        .and_then(|()| {
                            let interned = self.store.intern_physical_domain(
                                self.backend_attestation.physical_domain(),
                            )?;
                            self.policy
                                .authorize_surface(&prepared, &interned.binding)?;
                            let (freshness, frame_epoch) =
                                self.store.mint_observation_clock(&prepared.surface)?;
                            let mut observation = observation;
                            observation.authority =
                                ObservationAuthority::bind(&prepared, frame_epoch, freshness)?;
                            Ok(observation)
                        });
                        match validated {
                            Ok(observation) => match self.commit_observation(run_id, observation) {
                                Ok(observation) => {
                                    if let Some(lease) = observation_lease.take() {
                                        if let Err(error) =
                                            self.store.bind_surface_lease_observation(
                                                &lease.lease_id,
                                                lease.revision,
                                                run_id,
                                                observation.authority.frame_epoch,
                                                observation.authority.freshness.tick,
                                                Utc::now(),
                                            )
                                        {
                                            self.fail_coordinated_observation(
                                                run_id,
                                                &lease,
                                                &observation,
                                                &error,
                                            );
                                            return self.finish_and_return(request_id, Err(error));
                                        }
                                    }
                                    Ok(observation)
                                }
                                Err(error) => {
                                    if let Some(lease) = observation_lease.take() {
                                        let _ = self.revoke_observation_lease(
                                            &lease,
                                            "observation_commit_failed",
                                        );
                                    }
                                    self.fail_inflight(run_id, "observe", &error)?;
                                    Err(error)
                                }
                            },
                            Err(error) => {
                                if let Some(lease) = observation_lease.take() {
                                    let _ = self.revoke_observation_lease(
                                        &lease,
                                        "observation_validation_failed",
                                    );
                                }
                                self.fail_inflight(run_id, "observe", &error)?;
                                Err(error)
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(lease) = observation_lease.take() {
                            let _ =
                                self.revoke_observation_lease(&lease, "observation_backend_failed");
                        }
                        self.fail_inflight(run_id, "observe", &error)?;
                        Err(error)
                    }
                }
            }
            (Err(error), _) => {
                if let Some(lease) = observation_lease.take() {
                    if let Err(cleanup_error) =
                        self.revoke_observation_lease(&lease, "observation_denied")
                    {
                        return self.finish_and_return(request_id, Err(cleanup_error));
                    }
                }
                self.record_denial(run_id, "observe", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    fn preflight_agent_surface_observation(
        &self,
        current: &ComputerRun,
        caller: &ComputerAuthorityToken,
        expected_version: u64,
    ) -> ComputerResult<Option<ComputerSurfaceLease>> {
        if current
            .initiating_principal
            .as_ref()
            .is_none_or(|principal| principal.agent_id().is_none())
        {
            return Ok(None);
        }
        ensure_version(current, expected_version)?;
        self.policy
            .authorize_observation(current, Utc::now(), caller.principal())?;
        self.with_active_agent_work(current, || {
            self.store
                .acquire_agent_surface_observation(&current.run_id, Utc::now())
        })?
        .map(Some)
        .ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Pending,
                "the Computer surface is queued behind another Agent",
            )
        })
    }

    fn revoke_observation_lease(
        &self,
        lease: &ComputerSurfaceLease,
        disposition: &str,
    ) -> ComputerResult<()> {
        self.store
            .revoke_surface_lease_before_dispatch(
                &lease.lease_id,
                lease.revision,
                disposition,
                Utc::now(),
            )
            .map(|_| ())
    }

    fn revoke_active_observation_lease(
        &self,
        run_id: &str,
        disposition: &str,
    ) -> ComputerResult<()> {
        let active = self
            .store
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
        if let Some(lease) = active.first() {
            self.revoke_observation_lease(lease, disposition)?;
        }
        Ok(())
    }

    fn fail_coordinated_observation(
        &self,
        run_id: &str,
        lease: &ComputerSurfaceLease,
        observation: &ComputerObservation,
        error: &ComputerError,
    ) {
        if let Err(cleanup_error) = self.revoke_observation_lease(lease, "frame_bind_failed") {
            eprintln!(
                "[grokptah] failed to revoke Computer observation lease {}: {cleanup_error}",
                lease.lease_id
            );
        }
        if let Err(store_error) = self.store.update_run(run_id, |run| {
            if run
                .current_observation
                .as_ref()
                .map(|current| &current.observation_id)
                == Some(&observation.observation_id)
            {
                run.last_error = Some(error.clone());
                run.current_observation = None;
                run.transition(ComputerRunState::Failed)?;
                revoke_authority(run);
                run.record_audit("observe", "frame_bind_failed", None, None, Some(error.code));
            }
            Ok(())
        }) {
            eprintln!(
                "[grokptah] failed to terminalize Computer Run {run_id} after observation fence failure: {store_error}"
            );
        }
    }

    pub async fn act(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action: ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        validate_id("run_id", run_id)?;
        validate_id("observation_id", observation_id)?;
        action.validate(&ComputerUseLimits::ceiling())?;
        let payload = json!({
            "runId": run_id,
            "expectedVersion": expected_version,
            "observationId": observation_id,
            "action": action,
        });
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        if let Some(replayed) =
            self.begin_mutation(request_id, "act", &payload, caller, Some(&current))?
        {
            return replayed;
        }

        // Agent actions are coordinated against a host-owned physical input
        // conflict domain. Perform the complete read-only policy check before
        // creating a durable lease, then repeat it while transitioning the Run
        // so a stale frame/grant/takeover race fails before injection.
        let mut surface_dispatch = match self.preflight_agent_surface_dispatch(
            &current,
            caller,
            expected_version,
            observation_id,
            &action,
            run_id,
            request_id,
            &payload,
        ) {
            Ok(lease) => lease,
            Err(error) => {
                if error.code != ComputerErrorCode::Pending
                    && current
                        .initiating_principal
                        .as_ref()
                        .is_some_and(|principal| principal.agent_id().is_some())
                {
                    if let Err(cleanup_error) =
                        self.revoke_active_observation_lease(run_id, "action_preflight_failed")
                    {
                        return self.finish_and_return(request_id, Err(cleanup_error));
                    }
                }
                self.record_denial(run_id, "act", Some(action.class()), &error);
                return self.finish_and_return(request_id, Err(error));
            }
        };

        let mut budget_error = None;
        let prepared = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                let now = Utc::now();
                if self.policy.run_limit_reached(run, now) {
                    let error = run_limit_error();
                    run.last_error = Some(error.clone());
                    run.transition(ComputerRunState::LimitReached)?;
                    revoke_authority(run);
                    run.record_audit(
                        "act",
                        "limit_reached",
                        Some(action.class()),
                        None,
                        Some(error.code),
                    );
                    budget_error = Some(error);
                    return Ok(());
                }
                let observation = run.current_observation.clone().ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::StaleObservation,
                        "computer run has no current observation",
                    )
                })?;
                if observation.observation_id != observation_id {
                    return Err(ComputerError::new(
                        ComputerErrorCode::StaleObservation,
                        "action observation id is stale",
                    ));
                }
                let live_fence = self.store.live_freshness(&run.surface)?;
                self.policy.authorize_action(
                    run,
                    &observation,
                    &action,
                    now,
                    caller.principal(),
                    &live_fence,
                )?;
                if !self.backend.capabilities().allows_action(&action) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "the backend does not support this action class",
                    ));
                }
                run.transition(ComputerRunState::Acting)?;
                run.record_audit(
                    "act",
                    "started",
                    Some(action.class()),
                    Some(observation_id.into()),
                    None,
                );
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));

        let result = match (prepared, budget_error) {
            (Ok(_), Some(error)) => {
                if let Some(lease) = surface_dispatch.take() {
                    if let Err(cleanup_error) = self.fail_pre_injection_dispatch(&lease, error.code)
                    {
                        return self.finish_and_return(request_id, Err(cleanup_error));
                    }
                }
                Err(error)
            }
            (Ok(prepared), None) => {
                let observation = prepared
                    .current_observation
                    .clone()
                    .expect("prepared action has an observation");
                let control_epoch = prepared.control_epoch;
                let injected_dispatch = match surface_dispatch.take() {
                    Some(lease) => match self.inject_surface_dispatch(&lease) {
                        Ok(injected) => Some(injected),
                        Err(error) => {
                            self.fail_inflight(run_id, "act", &error)?;
                            return self.finish_and_return(request_id, Err(error));
                        }
                    },
                    None => None,
                };
                let outcome = self
                    .backend
                    .act_if_current(run_id, &observation, &action)
                    .await;
                match outcome {
                    Ok(outcome) => {
                        if let Some(lease) = injected_dispatch {
                            let outcome_value = match serde_json::to_value(&outcome) {
                                Ok(value) => value,
                                Err(_) => {
                                    let error = self.mark_agent_dispatch_uncertain(
                                        run_id,
                                        &lease,
                                        ComputerErrorCode::Internal,
                                    );
                                    return self.finish_and_return(request_id, Err(error));
                                }
                            };
                            let outcome_sha256 = crate::orchestration::hash_payload(&outcome_value);
                            if self
                                .store
                                .acknowledge_surface_dispatch(
                                    &lease.lease_id,
                                    lease.revision,
                                    dispatch_id(&lease)?,
                                    &outcome_sha256,
                                    Utc::now(),
                                )
                                .is_err()
                            {
                                let error = self.mark_agent_dispatch_uncertain(
                                    run_id,
                                    &lease,
                                    ComputerErrorCode::Internal,
                                );
                                return self.finish_and_return(request_id, Err(error));
                            }
                        }
                        self.commit_action(run_id, &action, &observation, control_epoch, outcome)
                    }
                    Err(error) => {
                        let error = if let Some(lease) = injected_dispatch {
                            self.mark_agent_dispatch_uncertain(run_id, &lease, error.code)
                        } else {
                            error
                        };
                        self.fail_inflight(run_id, "act", &error)?;
                        Err(error)
                    }
                }
            }
            (Err(error), _) => {
                if let Some(lease) = surface_dispatch.take() {
                    if let Err(cleanup_error) = self.fail_pre_injection_dispatch(&lease, error.code)
                    {
                        return self.finish_and_return(request_id, Err(cleanup_error));
                    }
                }
                self.record_denial(run_id, "act", Some(action.class()), &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn preflight_agent_surface_dispatch(
        &self,
        current: &ComputerRun,
        caller: &ComputerAuthorityToken,
        expected_version: u64,
        observation_id: &str,
        action: &ComputerAction,
        run_id: &str,
        request_id: &str,
        payload: &serde_json::Value,
    ) -> ComputerResult<Option<ComputerSurfaceLease>> {
        if current
            .initiating_principal
            .as_ref()
            .is_none_or(|principal| principal.agent_id().is_none())
        {
            return Ok(None);
        }
        let observation = current.current_observation.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "computer run has no current observation",
            )
        })?;
        ensure_version(current, expected_version)?;
        if observation.observation_id != observation_id {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "action observation id is stale",
            ));
        }
        let live_fence = self.store.live_freshness(&current.surface)?;
        self.policy.authorize_action(
            current,
            observation,
            action,
            Utc::now(),
            caller.principal(),
            &live_fence,
        )?;
        if !self.backend.capabilities().allows_action(action) {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "the backend does not support this action class",
            ));
        }
        let payload_sha256 = crate::orchestration::hash_payload(payload);
        match self.with_active_agent_work(current, || {
            self.store.acquire_agent_surface_dispatch(
                run_id,
                request_id,
                &payload_sha256,
                Utc::now(),
            )
        })? {
            ComputerDispatchClaim::Perform(lease) => Ok(Some(lease)),
            ComputerDispatchClaim::Pending => Err(ComputerError::new(
                ComputerErrorCode::Pending,
                "the Computer surface is queued behind another Agent",
            )),
            ComputerDispatchClaim::Uncertain | ComputerDispatchClaim::Replay(_) => {
                Err(ComputerError::new(
                    ComputerErrorCode::UncertainOutcome,
                    "the physical Computer action already crossed a durable dispatch boundary and will not be replayed",
                ))
            }
        }
    }

    fn inject_surface_dispatch(
        &self,
        lease: &ComputerSurfaceLease,
    ) -> ComputerResult<ComputerSurfaceLease> {
        let dispatch_id = dispatch_id(lease)?;
        let run = self
            .store
            .load_run(&lease.run_id)?
            .ok_or_else(unknown_run)?;
        match self.with_active_agent_work(&run, || {
            self.store.mark_surface_dispatch_injected(
                &lease.lease_id,
                lease.revision,
                dispatch_id,
                Utc::now(),
            )
        }) {
            Ok(injected) => Ok(injected),
            Err(error) => {
                let _ = self.store.fail_surface_dispatch(
                    &lease.lease_id,
                    dispatch_id,
                    error.code,
                    Utc::now(),
                );
                Err(error)
            }
        }
    }

    fn fail_pre_injection_dispatch(
        &self,
        lease: &ComputerSurfaceLease,
        error_code: ComputerErrorCode,
    ) -> ComputerResult<()> {
        self.store
            .fail_surface_dispatch(&lease.lease_id, dispatch_id(lease)?, error_code, Utc::now())
            .map(|_| ())
    }

    fn mark_agent_dispatch_uncertain(
        &self,
        run_id: &str,
        lease: &ComputerSurfaceLease,
        source_error_code: ComputerErrorCode,
    ) -> ComputerError {
        let uncertain = uncertain_dispatch_error();
        if let Ok(dispatch_id) = dispatch_id(lease) {
            if let Err(store_error) = self.store.fail_surface_dispatch(
                &lease.lease_id,
                dispatch_id,
                source_error_code,
                Utc::now(),
            ) {
                eprintln!(
                    "[grokptah] failed to mark injected Computer dispatch uncertain for run {run_id}, lease {}: {store_error}",
                    lease.lease_id
                );
            }
        }
        if let Err(store_error) = self.fail_inflight(run_id, "act", &uncertain) {
            eprintln!(
                "[grokptah] failed to terminalize Computer Run {run_id} after uncertain physical dispatch: {store_error}"
            );
        }
        uncertain
    }

    fn finish_and_return<T: Serialize>(
        &self,
        request_id: &str,
        result: ComputerResult<T>,
    ) -> ComputerResult<T> {
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Out-of-band operator control. Authorization is evaluated against the
    /// current durable Run while the store is locked; a stale UI snapshot can
    /// never turn Pause into an optimistic-concurrency conflict.
    pub async fn pause(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({ "runId": run_id });
        if let Some(replayed) =
            self.begin_mutation(request_id, "pause", &payload, caller, Some(&current))?
        {
            return replayed;
        }
        let paused = self
            .store
            .update_run_and_revoke_surface_leases(run_id, "paused", Utc::now(), |run| {
                self.require_caller(run, caller)?;
                if run.control_disposition == ComputerControlDisposition::OperatorTakeover {
                    return Err(ComputerError::new(
                        ComputerErrorCode::InvalidState,
                        "operator takeover is absorbing; create a new computer run",
                    ));
                }
                run.transition(ComputerRunState::Paused)?;
                revoke_authority(run);
                run.set_control_disposition(ComputerControlDisposition::Paused);
                run.record_audit("pause", "paused", None, None, None);
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        let result = match paused {
            Ok(run) => self.backend.cancel(run_id).await.map(|()| run),
            Err(error) => {
                self.record_denial(run_id, "pause", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Background-safe app-shell entry point. The opaque token cannot enter
    /// observation, grant, approval, or action APIs; it is downgraded to the
    /// owning local principal only inside this revoking transition.
    pub async fn emergency_pause(
        &self,
        request_id: &str,
        caller: &ComputerEmergencyControlToken,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        self.pause(request_id, &caller.authority(), run_id).await
    }

    /// Yields durable operator control. Authorization is evaluated against the
    /// current durable Run while the store is locked, never a client-held Run
    /// version. This is bookkeeping-safe takeover: it revokes grants, bumps
    /// epochs, and signals backend cancellation without relying on the caller's
    /// stale version. The macOS backend can now preempt its native preflight and
    /// activation wait; an atomic Accessibility call already entered into the
    /// operating system remains uncertain because it cannot be rolled back.
    pub async fn take_over(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({ "runId": run_id });
        if let Some(replayed) =
            self.begin_mutation(request_id, "take_over", &payload, caller, Some(&current))?
        {
            return replayed;
        }
        let taken_over = self
            .store
            .update_run_and_revoke_surface_leases(run_id, "operator_takeover", Utc::now(), |run| {
                self.require_takeover_caller(run, caller)?;
                run.transition(ComputerRunState::Paused)?;
                revoke_authority(run);
                run.set_control_disposition(ComputerControlDisposition::OperatorTakeover);
                run.record_audit("take_over", "operator_control", None, None, None);
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        let result = match taken_over {
            Ok(run) => self.backend.cancel(run_id).await.map(|()| run),
            Err(error) => {
                self.record_denial(run_id, "take_over", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Background-safe app-shell takeover; see [`Self::emergency_pause`].
    pub async fn emergency_take_over(
        &self,
        request_id: &str,
        caller: &ComputerEmergencyControlToken,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        self.take_over(request_id, &caller.authority(), run_id)
            .await
    }

    /// Explicitly quarantine an uncertain physical dispatch after the local
    /// operator has verified the exact surface incarnation is clear. The
    /// durable dispatch remains uncertain and is never replayed; this only
    /// releases the conflict-domain poison fence.
    #[allow(clippy::too_many_arguments)]
    pub fn reconcile_uncertain_surface_lease(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        lease_id: &str,
        expected_revision: u64,
        surface_id: &str,
        incarnation: &str,
        note: &str,
    ) -> ComputerResult<serde_json::Value> {
        validate_id("lease_id", lease_id)?;
        validate_id("surface_id", surface_id)?;
        validate_id("incarnation", incarnation)?;
        let lease = self.store.load_surface_lease(lease_id)?.ok_or_else(|| {
            ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown lease")
        })?;
        let run = self
            .store
            .load_run(&lease.run_id)?
            .ok_or_else(unknown_run)?;
        let payload = json!({
            "leaseId": lease_id,
            "expectedRevision": expected_revision,
            "surfaceId": surface_id,
            "incarnation": incarnation,
            "note": note,
        });
        if let Some(replayed) = self.begin_mutation(
            request_id,
            "reconcile_uncertain_surface_lease",
            &payload,
            caller,
            Some(&run),
        )? {
            return replayed;
        }
        let result = self
            .store
            .reconcile_uncertain_surface_lease(
                lease_id,
                expected_revision,
                surface_id,
                incarnation,
                note,
                Utc::now(),
            )
            .and_then(|lease| {
                serde_json::to_value(lease).map_err(|error| {
                    ComputerError::new(ComputerErrorCode::Internal, error.to_string())
                })
            });
        if let Err(error) = &result {
            self.record_denial(
                &lease.run_id,
                "reconcile_uncertain_surface_lease",
                None,
                error,
            );
        }
        self.finish_mutation(request_id, &result)?;
        result
    }

    pub async fn cancel(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({ "runId": run_id });
        if let Some(replayed) =
            self.begin_mutation(request_id, "cancel", &payload, caller, Some(&current))?
        {
            return replayed;
        }
        let cancelled = self
            .store
            .update_run_and_revoke_surface_leases(run_id, "cancelled", Utc::now(), |run| {
                self.require_caller(run, caller)?;
                if !run.state.is_terminal() {
                    run.transition(ComputerRunState::Cancelled)?;
                    revoke_authority(run);
                    run.set_control_disposition(ComputerControlDisposition::Stopped);
                    run.record_audit("cancel", "cancelled", None, None, None);
                }
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        let result = match cancelled {
            Ok(run) => self.backend.cancel(run_id).await.map(|()| run),
            Err(error) => {
                self.record_denial(run_id, "cancel", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Background-safe app-shell stop; see [`Self::emergency_pause`].
    pub async fn emergency_cancel(
        &self,
        request_id: &str,
        caller: &ComputerEmergencyControlToken,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        self.cancel(request_id, &caller.authority(), run_id).await
    }

    pub fn complete(
        &self,
        request_id: &str,
        caller: &ComputerAuthorityToken,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let current = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let payload = json!({ "runId": run_id, "expectedVersion": expected_version });
        if let Some(replayed) =
            self.begin_mutation(request_id, "complete", &payload, caller, Some(&current))?
        {
            return replayed;
        }
        let result = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                self.require_caller(run, caller)?;
                run.transition(ComputerRunState::Completed)?;
                revoke_authority(run);
                run.record_audit("complete", "completed", None, None, None);
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        if let Err(error) = &result {
            self.record_denial(run_id, "complete", None, error);
        }
        self.finish_mutation(request_id, &result)?;
        result
    }

    fn commit_observation(
        &self,
        run_id: &str,
        observation: ComputerObservation,
    ) -> ComputerResult<ComputerObservation> {
        let evidence_bytes = observation
            .screenshot
            .as_ref()
            .map_or(0, |evidence| evidence.byte_len);
        let mut limit_error = None;
        self.store
            .update_run(run_id, |run| {
                if run.state != ComputerRunState::Observing {
                    return Err(ComputerError::new(
                        ComputerErrorCode::Interrupted,
                        "observation was cancelled or superseded",
                    ));
                }
                if observation.target != run.target {
                    return Err(ComputerError::new(
                        ComputerErrorCode::TargetChanged,
                        "backend observed a different target",
                    ));
                }
                if observation.authority.surface != run.surface
                    || observation.authority.authority_epoch != run.authority_epoch
                    || observation.authority.control_epoch != run.control_epoch
                    || observation.authority.target_generation != run.target.generation
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::StaleObservation,
                        "observation is not bound to the live surface incarnation and authority epoch",
                    ));
                }
                run.freshness_tick = observation.authority.freshness.tick;
                if run
                    .current_observation
                    .as_ref()
                    .is_some_and(|current| {
                        observation.authority.frame_epoch <= current.authority.frame_epoch
                    })
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::StaleObservation,
                        "host frame epoch is not monotonic",
                    ));
                }
                if run.evidence_bytes.saturating_add(evidence_bytes) > run.limits.max_evidence_bytes
                {
                    let error = ComputerError::new(
                        ComputerErrorCode::LimitReached,
                        "computer-use evidence limit reached",
                    );
                    run.last_error = Some(error.clone());
                    run.transition(ComputerRunState::LimitReached)?;
                    revoke_authority(run);
                    run.record_audit("observe", "limit_reached", None, None, Some(error.code));
                    limit_error = Some(error);
                    return Ok(());
                }
                run.evidence_bytes = run.evidence_bytes.saturating_add(evidence_bytes);
                run.current_observation = Some(observation.clone());
                run.last_error = None;
                run.transition(ComputerRunState::Ready)?;
                run.record_audit(
                    "observe",
                    "completed",
                    None,
                    Some(observation.observation_id.clone()),
                    None,
                );
                Ok(())
            })?
            .ok_or_else(unknown_run)?;
        if let Some(error) = limit_error {
            return Err(error);
        }
        Ok(observation)
    }

    fn commit_action(
        &self,
        run_id: &str,
        action: &ComputerAction,
        observation: &ComputerObservation,
        control_epoch: u64,
        outcome: ActionOutcome,
    ) -> ComputerResult<ActionOutcome> {
        let mut uncertain_error = None;
        self.store
            .update_run(run_id, |run| {
                if run.state != ComputerRunState::Acting || run.control_epoch != control_epoch {
                    let error = ComputerError::new(
                        ComputerErrorCode::UncertainOutcome,
                        "action completed after the run was cancelled or superseded",
                    );
                    run.last_error = Some(error.clone());
                    run.record_audit(
                        "act",
                        "uncertain_outcome",
                        Some(action.class()),
                        Some(observation.observation_id.clone()),
                        Some(error.code),
                    );
                    uncertain_error = Some(error);
                    return Ok(());
                }
                run.action_count = run.action_count.saturating_add(1);
                if let Some(grant) = &mut run.grant {
                    if let Some(remaining) = &mut grant.uses_remaining {
                        *remaining = remaining.saturating_sub(1);
                    }
                }
                run.last_outcome = Some(outcome.clone());
                run.current_observation = None;
                run.last_error = None;
                let grant_exhausted = run
                    .grant
                    .as_ref()
                    .is_some_and(|grant| grant.uses_remaining == Some(0));
                if run.action_count >= run.limits.max_actions {
                    run.transition(ComputerRunState::LimitReached)?;
                    revoke_authority(run);
                } else if grant_exhausted {
                    run.transition(ComputerRunState::Paused)?;
                    revoke_authority(run);
                    run.set_control_disposition(ComputerControlDisposition::Paused);
                } else {
                    run.transition(ComputerRunState::Ready)?;
                }
                run.record_audit(
                    "act",
                    "completed",
                    Some(action.class()),
                    Some(observation.observation_id.clone()),
                    None,
                );
                Ok(())
            })?
            .ok_or_else(unknown_run)?;
        if let Some(error) = uncertain_error {
            return Err(error);
        }
        Ok(outcome)
    }

    fn fail_inflight(
        &self,
        run_id: &str,
        operation: &str,
        error: &ComputerError,
    ) -> ComputerResult<()> {
        self.store.update_run(run_id, |run| {
            if matches!(
                run.state,
                ComputerRunState::Observing | ComputerRunState::Acting
            ) {
                run.last_error = Some(error.clone());
                run.transition(ComputerRunState::Failed)?;
                revoke_authority(run);
                if error.code == ComputerErrorCode::UncertainOutcome {
                    run.set_control_disposition(ComputerControlDisposition::UncertainOutcome);
                }
                run.record_audit(
                    operation,
                    "failed",
                    None,
                    run.current_observation
                        .as_ref()
                        .map(|observation| observation.observation_id.clone()),
                    Some(error.code),
                );
            }
            Ok(())
        })?;
        Ok(())
    }

    fn record_denial(
        &self,
        run_id: &str,
        operation: &str,
        action_class: Option<super::types::ActionClass>,
        error: &ComputerError,
    ) {
        let _ = self.store.update_run(run_id, |run| {
            run.updated_at = Utc::now();
            run.record_audit(operation, "denied", action_class, None, Some(error.code));
            Ok(())
        });
    }

    fn begin_mutation<T: DeserializeOwned>(
        &self,
        request_id: &str,
        operation: &str,
        payload: &serde_json::Value,
        caller: &ComputerAuthorityToken,
        run: Option<&ComputerRun>,
    ) -> ComputerResult<Option<ComputerResult<T>>> {
        let hash = crate::orchestration::hash_payload(payload);
        let stamp = MutationStamp::from_caller(caller.principal().clone(), run);
        // Authorize before any receipt lookup so a unique unauthorized
        // request id creates no durable receipt or audit-capacity side
        // effect and cannot receive another principal's cached result.
        self.authorize_new_mutation(operation, caller, run)?;
        // Receipt possession is not continuing authority. Revalidate an
        // Agent's exact live WorkAttempt before returning either a success or
        // failure replay, so cancellation, lease expiry, reassignment, or a
        // spec revision also revokes access to cached observations/outcomes.
        if caller.principal().agent_id().is_some() {
            if let Some(run) = run {
                self.with_active_agent_work(run, || Ok(()))?;
            }
        }
        if let Some(replayed) = self
            .store
            .replay_mutation(request_id, operation, &hash, &stamp)?
        {
            return Ok(Some(match replayed {
                Ok(value) => serde_json::from_value(value).map_err(|error| {
                    ComputerError::new(ComputerErrorCode::Internal, error.to_string())
                }),
                Err(error) => Err(error),
            }));
        }
        match self
            .store
            .claim_mutation(request_id, operation, &hash, &stamp)?
        {
            MutationClaim::Perform => Ok(None),
            MutationClaim::Pending => Ok(Some(Err(ComputerError::new(
                ComputerErrorCode::Pending,
                "an identical computer-use mutation is in progress",
            )))),
            MutationClaim::Uncertain => Ok(Some(Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "the earlier computer-use mutation has an uncertain outcome and will not be retried",
            )))),
            MutationClaim::Replay(result) => {
                let decoded = match result {
                    Ok(value) => serde_json::from_value(value).map_err(|error| {
                        ComputerError::new(ComputerErrorCode::Internal, error.to_string())
                    }),
                    Err(error) => Err(error),
                };
                Ok(Some(decoded))
            }
        }
    }

    fn authorize_new_mutation(
        &self,
        operation: &str,
        caller: &ComputerAuthorityToken,
        run: Option<&ComputerRun>,
    ) -> ComputerResult<()> {
        caller.principal().validate()?;
        match operation {
            "create_run" => caller
                .principal()
                .session_id()
                .filter(|session_id| !session_id.is_nil())
                .map(|_| ())
                .ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "create_run requires a host-issued local operator session",
                    )
                }),
            "create_agent_run" => caller
                .principal()
                .agent_id()
                .zip(caller.principal().agent_spec_revision())
                .map(|_| ())
                .ok_or_else(|| {
                    ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "create_agent_run requires a host-issued durable Agent principal",
                    )
                }),
            "reconcile_uncertain_surface_lease" => {
                let run = run.ok_or_else(unknown_run)?;
                if caller.principal().public_kind() != "local_operator_session"
                    || caller.principal().session_id() != Some(run.owner_session_id)
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "surface reconciliation requires the owning local operator",
                    ));
                }
                Ok(())
            }
            "take_over" => {
                let run = run.ok_or_else(unknown_run)?;
                if caller.principal().session_id() != Some(run.owner_session_id) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::Unauthorized,
                        "operator takeover requires the Run's host-resolved owner Lane",
                    ));
                }
                Ok(())
            }
            _ => {
                let run = run.ok_or_else(unknown_run)?;
                self.require_caller(run, caller)
            }
        }
    }

    fn require_takeover_caller(
        &self,
        run: &ComputerRun,
        caller: &ComputerAuthorityToken,
    ) -> ComputerResult<()> {
        if caller.principal().session_id() != Some(run.owner_session_id) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "operator takeover requires the Run's host-resolved owner Lane",
            ));
        }
        Ok(())
    }

    fn finish_mutation<T: Serialize>(
        &self,
        request_id: &str,
        result: &ComputerResult<T>,
    ) -> ComputerResult<()> {
        let encoded = match result {
            Ok(value) => serde_json::to_value(value).map_err(|error| {
                ComputerError::new(ComputerErrorCode::Internal, error.to_string())
            }),
            Err(error) => Err(error.clone()),
        };
        self.store.complete_mutation(request_id, &encoded)
    }
}

fn ensure_version(run: &ComputerRun, expected_version: u64) -> ComputerResult<()> {
    if run.version != expected_version {
        return Err(ComputerError::new(
            ComputerErrorCode::Conflict,
            format!(
                "stale computer run version: expected {expected_version}, current {}",
                run.version
            ),
        ));
    }
    Ok(())
}

fn revoke_authority(run: &mut ComputerRun) {
    if let Some(grant) = &mut run.grant {
        grant.revoked_at.get_or_insert_with(Utc::now);
    }
    run.current_observation = None;
    run.bump_authority_epoch();
}

fn unknown_run() -> ComputerError {
    ComputerError::new(ComputerErrorCode::InvalidRequest, "unknown computer run")
}

fn dispatch_id(lease: &ComputerSurfaceLease) -> ComputerResult<&str> {
    lease
        .dispatch
        .as_ref()
        .map(|dispatch| dispatch.dispatch_id.as_str())
        .ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Internal,
                "surface lease is missing its physical dispatch record",
            )
        })
}

fn uncertain_dispatch_error() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::UncertainOutcome,
        "the physical Computer action crossed the injection boundary but its durable outcome is uncertain; it will not be replayed",
    )
}

fn run_limit_error() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::LimitReached,
        "computer-use action or duration limit reached",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Duration;
    use tempfile::tempdir;
    use tokio::sync::Notify;

    use super::*;
    use crate::computer_use::coordination::{ComputerDispatchState, ComputerSurfaceLeaseState};
    use crate::computer_use::{ActionClass, ComputerCapabilities, EvidenceRef, SimulatorBackend};

    #[derive(Debug, Default)]
    struct BlockingBackend {
        inner: SimulatorBackend,
        action_entered: Notify,
        release_action: Notify,
        action_calls: AtomicUsize,
    }

    #[derive(Debug)]
    struct EvidenceBackend {
        inner: SimulatorBackend,
        bytes: parking_lot::Mutex<Vec<u8>>,
    }

    #[derive(Debug, Default)]
    struct MismatchedObservationBackend {
        inner: SimulatorBackend,
    }

    impl Default for EvidenceBackend {
        fn default() -> Self {
            Self {
                inner: SimulatorBackend::new(),
                bytes: parking_lot::Mutex::new(b"ok".to_vec()),
            }
        }
    }

    fn trusted_fixture_service(
        backend: Arc<dyn ComputerBackend>,
        store: ComputerStore,
    ) -> ComputerUseService {
        let mut capabilities = backend.capabilities();
        capabilities.hydrate_legacy();
        let attestation = ComputerBackendAttestation::trusted(
            capabilities.proof.backend_id(),
            capabilities.proof.tier(),
            backend.physical_input_domain(),
        )
        .expect("trusted test backend registration is valid");
        ComputerUseService::new_trusted(backend, store, attestation)
    }

    #[async_trait::async_trait]
    impl ComputerBackend for EvidenceBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            self.inner.capabilities()
        }

        fn physical_input_domain(&self) -> crate::computer_use::PhysicalInputDomain {
            self.inner.physical_input_domain()
        }

        async fn observe(
            &self,
            run_id: &str,
            observation_id: &str,
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            let mut observation = self
                .inner
                .observe(run_id, observation_id, target, limits)
                .await?;
            let bytes = self.bytes.lock();
            observation.screenshot = Some(EvidenceRef {
                content_sha256: format!("{:x}", Sha256::digest(&*bytes)),
                media_type: "image/png".into(),
                byte_len: bytes.len() as u64,
                width: 800,
                height: 600,
                redacted: true,
                asset_id: "simulated-redacted-evidence".into(),
            });
            Ok(observation)
        }

        async fn read_evidence(
            &self,
            _run_id: &str,
            _asset_id: &str,
        ) -> ComputerResult<Option<Vec<u8>>> {
            // Deliberately permissive fake: the service must enforce current
            // run/asset scope and integrity independently of its backend.
            Ok(Some(self.bytes.lock().clone()))
        }

        async fn act(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.inner.act(run_id, observation, action).await
        }

        async fn act_if_current(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.inner.act_if_current(run_id, observation, action).await
        }

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.inner.cancel(run_id).await
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for BlockingBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            self.inner.capabilities()
        }

        fn physical_input_domain(&self) -> crate::computer_use::PhysicalInputDomain {
            self.inner.physical_input_domain()
        }

        async fn observe(
            &self,
            run_id: &str,
            observation_id: &str,
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            self.inner
                .observe(run_id, observation_id, target, limits)
                .await
        }

        async fn act(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.action_calls.fetch_add(1, Ordering::SeqCst);
            self.action_entered.notify_one();
            self.release_action.notified().await;
            self.inner.act(run_id, observation, action).await
        }

        async fn act_if_current(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.action_calls.fetch_add(1, Ordering::SeqCst);
            self.action_entered.notify_one();
            self.release_action.notified().await;
            self.inner.act_if_current(run_id, observation, action).await
        }

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.release_action.notify_waiters();
            self.inner.cancel(run_id).await
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for MismatchedObservationBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            self.inner.capabilities()
        }

        fn physical_input_domain(&self) -> crate::computer_use::PhysicalInputDomain {
            self.inner.physical_input_domain()
        }

        async fn observe(
            &self,
            run_id: &str,
            observation_id: &str,
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            let mut observation = self
                .inner
                .observe(run_id, observation_id, target, limits)
                .await?;
            observation.observation_id = "PRIVATE_BACKEND_OBSERVATION_ID".into();
            Ok(observation)
        }

        async fn act(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.inner.act(run_id, observation, action).await
        }

        async fn act_if_current(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.inner.act_if_current(run_id, observation, action).await
        }

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.inner.cancel(run_id).await
        }
    }

    fn service() -> (Arc<SimulatorBackend>, ComputerUseService) {
        let dir = tempdir().unwrap().keep();
        let backend = Arc::new(SimulatorBackend::new());
        let service = ComputerUseService::new_simulator(
            backend.clone(),
            ComputerStore::open(dir.join("computer-use")).unwrap(),
        );
        (backend, service)
    }

    fn grant(run: &ComputerRun) -> ActionGrant {
        let now = Utc::now();
        ActionGrant::for_run(
            run,
            BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            now,
            now + Duration::minutes(5),
            Some(8),
        )
    }

    fn caller(
        run: &ComputerRun,
        _service: &ComputerUseService,
    ) -> crate::computer_use::ComputerAuthorityToken {
        ComputerAuthorityToken::local_operator(run.owner_session_id)
            .expect("owner session is a valid local operator")
    }

    fn create_authorized_agent_run(
        service: &ComputerUseService,
        suffix: &str,
    ) -> (ComputerRun, ComputerAuthorityToken) {
        service.trust_unbound_agent_work_for_tests();
        let agent_id = format!("agent-{suffix}");
        let token = ComputerAuthorityToken::agent_from_host_record(&agent_id, 1).unwrap();
        let run = service
            .create_agent_run(
                &token,
                ResolvedAgentComputerRunAdmission {
                    request_id: format!("create-agent-run-{suffix}"),
                    owner_session_id: Uuid::new_v4(),
                    binding: crate::computer_use::ComputerWorkAttemptBinding {
                        work_id: format!("work-{suffix}"),
                        work_attempt_id: format!("attempt-{suffix}"),
                        agent_id,
                        agent_spec_revision: 1,
                    },
                    workspace: format!("/tmp/workspace-{suffix}"),
                    target: SimulatorBackend::demo_target(),
                    limits: ComputerUseLimits::default(),
                },
            )
            .unwrap();
        let run = service
            .authorize(
                &format!("authorize-agent-run-{suffix}"),
                &token,
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        (run, token)
    }

    async fn observe_agent_run(
        service: &ComputerUseService,
        run: &ComputerRun,
        token: &ComputerAuthorityToken,
        suffix: &str,
    ) -> ComputerResult<ComputerObservation> {
        service
            .observe(
                &format!("observe-agent-run-{suffix}"),
                token,
                &run.run_id,
                service
                    .get_run(&run.run_id)?
                    .ok_or_else(unknown_run)?
                    .version,
            )
            .await
    }

    #[tokio::test]
    async fn agent_attention_events_are_idempotent_and_do_not_grant_or_dispatch() {
        let (backend, service) = service();
        let owner_session_id = Uuid::new_v4();
        let token = ComputerAuthorityToken::local_operator(owner_session_id).unwrap();
        let run = service
            .create_run(
                "create-attention-run",
                &token,
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "authorize-attention-run",
                &token,
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe("observe-attention-run", &token, &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let action = ComputerAction::SetValue {
            element_id: format!("{}-name", observation.observation_id),
            text: "not persisted in attention evidence".into(),
        };
        let attention = ComputerAttentionPoint::for_action(&observation, &action);
        let staged = service
            .record_agent_action_proposal(
                "stage-agent-attention",
                &token,
                &run.run_id,
                current.version,
                &observation.observation_id,
                action.class(),
                attention,
            )
            .unwrap();

        assert_eq!(staged.version, current.version);
        assert_eq!(staged.action_count, 0);
        assert_eq!(backend.action_attempt_count(), 0);
        assert_eq!(
            staged
                .audit
                .iter()
                .rev()
                .take(3)
                .map(|entry| entry.surface_event)
                .collect::<Vec<_>>(),
            vec![
                ComputerSurfaceEvent::ApprovalRequired,
                ComputerSurfaceEvent::AttentionMoved,
                ComputerSurfaceEvent::ActionProposed,
            ]
        );
        assert!(!serde_json::to_string(&staged.audit)
            .unwrap()
            .contains("not persisted in attention evidence"));

        let replayed = service
            .record_agent_action_proposal(
                "stage-agent-attention",
                &token,
                &run.run_id,
                current.version,
                &observation.observation_id,
                action.class(),
                attention,
            )
            .unwrap();
        assert_eq!(replayed.audit.len(), staged.audit.len());

        let rejected = service
            .record_agent_approval_rejected(
                "reject-agent-attention",
                &token,
                &run.run_id,
                current.version,
                &observation.observation_id,
                action.class(),
            )
            .unwrap();
        assert_eq!(rejected.version, current.version);
        assert_eq!(rejected.action_count, 0);
        assert_eq!(
            rejected.audit.last().unwrap().surface_event,
            ComputerSurfaceEvent::ApprovalRejected
        );
        assert!(rejected.audit.last().unwrap().attention.is_none());
        assert_eq!(backend.action_attempt_count(), 0);
    }

    #[tokio::test]
    async fn same_domain_agents_serialize_observation_and_physical_dispatch() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let (run_a, token_a) = create_authorized_agent_run(&service, "serial-a");
        let (run_b, token_b) = create_authorized_agent_run(&service, "serial-b");
        let (run_c, token_c) = create_authorized_agent_run(&service, "serial-c");

        let observation_a = observe_agent_run(&service, &run_a, &token_a, "serial-a")
            .await
            .unwrap();
        let current_a = service.get_run(&run_a.run_id).unwrap().unwrap();
        let action_service = service.clone();
        let action_run_id = run_a.run_id.clone();
        let action_observation = observation_a.clone();
        let action = tokio::spawn(async move {
            action_service
                .act(
                    "act-agent-run-serial-a",
                    &token_a,
                    &action_run_id,
                    current_a.version,
                    &action_observation.observation_id,
                    ComputerAction::SetValue {
                        element_id: format!("{}-name", action_observation.observation_id),
                        text: "Ada".into(),
                    },
                )
                .await
        });
        backend.action_entered.notified().await;

        let pending = observe_agent_run(&service, &run_b, &token_b, "serial-b-pending")
            .await
            .unwrap_err();
        assert_eq!(pending.code, ComputerErrorCode::Pending);
        let pending = observe_agent_run(&service, &run_c, &token_c, "serial-c-pending")
            .await
            .unwrap_err();
        assert_eq!(pending.code, ComputerErrorCode::Pending);
        let queued = service
            .store
            .list_surface_leases()
            .unwrap()
            .into_iter()
            .filter(|lease| lease.state == ComputerSurfaceLeaseState::Queued)
            .collect::<Vec<_>>();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].run_id, run_b.run_id);
        assert_eq!(queued[1].run_id, run_c.run_id);
        assert!(queued[0].queue_sequence < queued[1].queue_sequence);
        assert_eq!(backend.action_calls.load(Ordering::SeqCst), 1);

        let coordination_a = service
            .local_surface_coordination(run_a.owner_session_id, &run_a.run_id, Utc::now())
            .unwrap()
            .unwrap();
        assert_eq!(
            coordination_a.state,
            ComputerSurfaceCoordinationState::Dispatching
        );
        assert!(coordination_a.owns_surface);
        assert_eq!(coordination_a.queue_position, None);
        assert_eq!(coordination_a.queue_depth, 2);
        assert_eq!(
            coordination_a
                .active
                .as_ref()
                .map(|active| active.agent_id.as_str()),
            Some("agent-serial-a")
        );

        let coordination_b = service
            .local_surface_coordination(run_b.owner_session_id, &run_b.run_id, Utc::now())
            .unwrap()
            .unwrap();
        assert_eq!(
            coordination_b.state,
            ComputerSurfaceCoordinationState::Queued
        );
        assert!(!coordination_b.owns_surface);
        assert_eq!(coordination_b.queue_position, Some(1));
        assert_eq!(coordination_b.queue_depth, 2);
        assert_eq!(
            coordination_b.active.as_ref().map(|active| &active.run_id),
            Some(&run_a.run_id)
        );
        let projected_json = serde_json::to_string(&coordination_b).unwrap();
        assert!(!projected_json.contains(&queued[0].lease_id));
        assert!(!projected_json.contains(&queued[0].work_attempt_id));
        assert!(!projected_json.contains(&queued[0].conflict_domain_id));

        let coordination_c = service
            .local_surface_coordination(run_c.owner_session_id, &run_c.run_id, Utc::now())
            .unwrap()
            .unwrap();
        assert_eq!(coordination_c.queue_position, Some(2));
        let error = service
            .local_surface_coordination(run_a.owner_session_id, &run_b.run_id, Utc::now())
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Unauthorized);

        backend.release_action.notify_one();
        action.await.unwrap().unwrap();

        let observation_b = observe_agent_run(&service, &run_b, &token_b, "serial-b-granted")
            .await
            .unwrap();
        let current_b = service.get_run(&run_b.run_id).unwrap().unwrap();
        backend.release_action.notify_one();
        service
            .act(
                "act-agent-run-serial-b",
                &token_b,
                &run_b.run_id,
                current_b.version,
                &observation_b.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation_b.observation_id),
                    text: "Grace".into(),
                },
            )
            .await
            .unwrap();
        let observation_c = observe_agent_run(&service, &run_c, &token_c, "serial-c-granted")
            .await
            .unwrap();
        let current_c = service.get_run(&run_c.run_id).unwrap().unwrap();
        backend.release_action.notify_one();
        service
            .act(
                "act-agent-run-serial-c",
                &token_c,
                &run_c.run_id,
                current_c.version,
                &observation_c.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation_c.observation_id),
                    text: "Linus".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(backend.action_calls.load(Ordering::SeqCst), 3);
        let leases = service.store.list_surface_leases().unwrap();
        assert_eq!(leases.len(), 3);
        assert!(leases
            .iter()
            .all(|lease| lease.state == ComputerSurfaceLeaseState::Released));
    }

    #[tokio::test]
    async fn local_surface_advance_stales_agent_frame_before_backend_dispatch() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(SimulatorBackend::new());
        let service = ComputerUseService::new_simulator(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let (agent_run, agent_token) = create_authorized_agent_run(&service, "stale-frame");
        let agent_observation =
            observe_agent_run(&service, &agent_run, &agent_token, "stale-frame")
                .await
                .unwrap();

        let operator = ComputerAuthorityToken::local_operator(agent_run.owner_session_id).unwrap();
        let local_run = service
            .create_run(
                "create-local-frame-advance",
                &operator,
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let local_run = service
            .authorize(
                "authorize-local-frame-advance",
                &operator,
                &local_run.run_id,
                local_run.version,
                grant(&local_run),
            )
            .unwrap();
        service
            .observe(
                "observe-local-frame-advance",
                &operator,
                &local_run.run_id,
                local_run.version,
            )
            .await
            .unwrap();

        let current = service.get_run(&agent_run.run_id).unwrap().unwrap();
        let error = service
            .act(
                "act-stale-agent-frame",
                &agent_token,
                &agent_run.run_id,
                current.version,
                &agent_observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", agent_observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
        assert_eq!(backend.action_attempt_count(), 0);
        let lease = service.store.list_surface_leases().unwrap().remove(0);
        assert_eq!(lease.state, ComputerSurfaceLeaseState::Revoked);
        assert!(lease.dispatch.is_none());
    }

    #[tokio::test]
    async fn operator_takeover_and_agent_injection_share_one_linearization_fence() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let (run, token) = create_authorized_agent_run(&service, "takeover-agent");
        let observation = observe_agent_run(&service, &run, &token, "takeover-agent")
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let action_service = service.clone();
        let action_run_id = run.run_id.clone();
        let action_observation = observation.clone();
        let action = tokio::spawn(async move {
            action_service
                .act(
                    "act-takeover-agent",
                    &token,
                    &action_run_id,
                    current.version,
                    &action_observation.observation_id,
                    ComputerAction::SetValue {
                        element_id: format!("{}-name", action_observation.observation_id),
                        text: "Ada".into(),
                    },
                )
                .await
        });
        backend.action_entered.notified().await;

        let operator = ComputerAuthorityToken::local_operator(run.owner_session_id).unwrap();
        let taken_over = service
            .take_over("take-over-agent-run", &operator, &run.run_id)
            .await
            .unwrap();
        assert_eq!(taken_over.state, ComputerRunState::Paused);
        assert_eq!(
            taken_over.control_disposition,
            ComputerControlDisposition::OperatorTakeover
        );
        let lease = service.store.list_surface_leases().unwrap().remove(0);
        assert_eq!(lease.state, ComputerSurfaceLeaseState::Uncertain);
        assert_eq!(
            lease.dispatch.as_ref().unwrap().state,
            ComputerDispatchState::Uncertain
        );

        backend.release_action.notify_one();
        let error = action.await.unwrap().unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
        assert_eq!(backend.action_calls.load(Ordering::SeqCst), 1);
        let terminal = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            terminal.control_disposition,
            ComputerControlDisposition::OperatorTakeover,
            "late completion cannot regain Agent control"
        );
    }

    #[tokio::test]
    async fn independently_isolated_agent_domains_can_hold_capacity_together() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let first_backend = Arc::new(SimulatorBackend::independently_isolated());
        let second_backend = Arc::new(SimulatorBackend::independently_isolated());
        let first = ComputerUseService::new_simulator(first_backend, store.clone());
        let second = ComputerUseService::new_simulator(second_backend, store.clone());
        let (run_a, token_a) = create_authorized_agent_run(&first, "isolated-a");
        let (run_b, token_b) = create_authorized_agent_run(&second, "isolated-b");

        let observation_a = observe_agent_run(&first, &run_a, &token_a, "isolated-a")
            .await
            .unwrap();
        let observation_b = observe_agent_run(&second, &run_b, &token_b, "isolated-b")
            .await
            .unwrap();
        let granted = store
            .list_surface_leases()
            .unwrap()
            .into_iter()
            .filter(|lease| lease.state == ComputerSurfaceLeaseState::Granted)
            .collect::<Vec<_>>();
        assert_eq!(granted.len(), 2);
        assert_ne!(granted[0].conflict_domain_id, granted[1].conflict_domain_id);

        let current_a = first.get_run(&run_a.run_id).unwrap().unwrap();
        first
            .act(
                "act-agent-run-isolated-a",
                &token_a,
                &run_a.run_id,
                current_a.version,
                &observation_a.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation_a.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        let current_b = second.get_run(&run_b.run_id).unwrap().unwrap();
        second
            .act(
                "act-agent-run-isolated-b",
                &token_b,
                &run_b.run_id,
                current_b.version,
                &observation_b.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation_b.observation_id),
                    text: "Grace".into(),
                },
            )
            .await
            .unwrap();
    }

    #[test]
    fn prepared_and_injected_agent_dispatches_recover_fail_closed_twice() {
        for injected in [false, true] {
            let dir = tempdir().unwrap();
            let (lease_id, dispatch_key) = {
                let backend = Arc::new(SimulatorBackend::new());
                let service = ComputerUseService::new_simulator(
                    backend,
                    ComputerStore::open(dir.path().join("computer-use")).unwrap(),
                );
                let (run, token) = create_authorized_agent_run(
                    &service,
                    if injected {
                        "restart-injected"
                    } else {
                        "restart-prepared"
                    },
                );
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime
                    .block_on(observe_agent_run(
                        &service,
                        &run,
                        &token,
                        if injected {
                            "restart-injected"
                        } else {
                            "restart-prepared"
                        },
                    ))
                    .unwrap();
                let payload_sha256 = crate::orchestration::hash_payload(
                    &json!({"runId": run.run_id, "action": "fixture"}),
                );
                let lease = match service
                    .store
                    .acquire_agent_surface_dispatch(
                        &run.run_id,
                        if injected {
                            "dispatch-injected"
                        } else {
                            "dispatch-prepared"
                        },
                        &payload_sha256,
                        Utc::now(),
                    )
                    .unwrap()
                {
                    ComputerDispatchClaim::Perform(lease) => lease,
                    other => panic!("expected a physical dispatch claim, got {other:?}"),
                };
                let dispatch_id = dispatch_id(&lease).unwrap().to_string();
                let lease = if injected {
                    service
                        .store
                        .mark_surface_dispatch_injected(
                            &lease.lease_id,
                            lease.revision,
                            &dispatch_id,
                            Utc::now(),
                        )
                        .unwrap()
                } else {
                    lease
                };
                (lease.lease_id, dispatch_id)
            };

            let first = ComputerStore::open(dir.path().join("computer-use")).unwrap();
            let recovered = first.load_surface_lease(&lease_id).unwrap().unwrap();
            let expected_lease_state = if injected {
                ComputerSurfaceLeaseState::Uncertain
            } else {
                ComputerSurfaceLeaseState::Revoked
            };
            let expected_dispatch_state = if injected {
                ComputerDispatchState::Uncertain
            } else {
                ComputerDispatchState::KnownNotInjected
            };
            assert_eq!(recovered.state, expected_lease_state);
            assert_eq!(dispatch_id(&recovered).unwrap(), dispatch_key);
            assert_eq!(
                recovered.dispatch.as_ref().unwrap().state,
                expected_dispatch_state
            );
            let first_revision = recovered.revision;
            drop(first);

            let second = ComputerStore::open(dir.path().join("computer-use")).unwrap();
            let stable = second.load_surface_lease(&lease_id).unwrap().unwrap();
            assert_eq!(stable.state, expected_lease_state);
            assert_eq!(stable.revision, first_revision);
            assert_eq!(
                stable.dispatch.as_ref().unwrap().state,
                expected_dispatch_state
            );
        }
    }

    #[tokio::test]
    async fn physical_dispatch_id_deduplicates_every_durable_boundary() {
        let dir = tempdir().unwrap();
        let service = ComputerUseService::new_simulator(
            Arc::new(SimulatorBackend::new()),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let (run, token) = create_authorized_agent_run(&service, "dispatch-dedup");
        observe_agent_run(&service, &run, &token, "dispatch-dedup")
            .await
            .unwrap();
        let payload_sha256 = crate::orchestration::hash_payload(&json!({"action": "one"}));
        let prepared = match service
            .store
            .acquire_agent_surface_dispatch(
                &run.run_id,
                "dispatch-request-dedup",
                &payload_sha256,
                Utc::now(),
            )
            .unwrap()
        {
            ComputerDispatchClaim::Perform(lease) => lease,
            other => panic!("expected Perform, got {other:?}"),
        };
        let dispatch_key = dispatch_id(&prepared).unwrap().to_string();
        assert!(matches!(
            service
                .store
                .prepare_surface_dispatch(
                    &prepared.lease_id,
                    prepared.revision,
                    &dispatch_key,
                    &payload_sha256,
                    Utc::now(),
                )
                .unwrap(),
            ComputerDispatchClaim::Pending
        ));
        assert_eq!(
            service
                .store
                .prepare_surface_dispatch(
                    &prepared.lease_id,
                    prepared.revision,
                    &dispatch_key,
                    &"b".repeat(64),
                    Utc::now(),
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );

        let injected = service
            .store
            .mark_surface_dispatch_injected(
                &prepared.lease_id,
                prepared.revision,
                &dispatch_key,
                Utc::now(),
            )
            .unwrap();
        assert!(matches!(
            service
                .store
                .prepare_surface_dispatch(
                    &injected.lease_id,
                    injected.revision,
                    &dispatch_key,
                    &payload_sha256,
                    Utc::now(),
                )
                .unwrap(),
            ComputerDispatchClaim::Uncertain
        ));
        let acknowledged = service
            .store
            .acknowledge_surface_dispatch(
                &injected.lease_id,
                injected.revision,
                &dispatch_key,
                &"c".repeat(64),
                Utc::now(),
            )
            .unwrap();
        assert!(matches!(
            service
                .store
                .prepare_surface_dispatch(
                    &acknowledged.lease_id,
                    acknowledged.revision,
                    &dispatch_key,
                    &payload_sha256,
                    Utc::now(),
                )
                .unwrap(),
            ComputerDispatchClaim::Replay(_)
        ));
        assert_eq!(service.store.list_surface_leases().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn uncertain_dispatch_poison_is_exact_to_its_physical_input_domain() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let shared =
            ComputerUseService::new_simulator(Arc::new(SimulatorBackend::new()), store.clone());
        let (run_a, token_a) = create_authorized_agent_run(&shared, "uncertain-domain-a");
        let (run_b, token_b) = create_authorized_agent_run(&shared, "uncertain-domain-b");
        observe_agent_run(&shared, &run_a, &token_a, "uncertain-domain-a")
            .await
            .unwrap();

        let payload_sha256 = crate::orchestration::hash_payload(&json!({"action": "uncertain"}));
        let prepared = match store
            .acquire_agent_surface_dispatch(
                &run_a.run_id,
                "uncertain-domain-dispatch",
                &payload_sha256,
                Utc::now(),
            )
            .unwrap()
        {
            ComputerDispatchClaim::Perform(lease) => lease,
            other => panic!("expected Perform, got {other:?}"),
        };
        let dispatch_key = dispatch_id(&prepared).unwrap().to_string();
        let injected = store
            .mark_surface_dispatch_injected(
                &prepared.lease_id,
                prepared.revision,
                &dispatch_key,
                Utc::now(),
            )
            .unwrap();
        let uncertain = store
            .fail_surface_dispatch(
                &injected.lease_id,
                &dispatch_key,
                ComputerErrorCode::Internal,
                Utc::now(),
            )
            .unwrap();
        assert_eq!(uncertain.state, ComputerSurfaceLeaseState::Uncertain);

        let blocked = observe_agent_run(&shared, &run_b, &token_b, "uncertain-domain-b")
            .await
            .unwrap_err();
        assert_eq!(blocked.code, ComputerErrorCode::UncertainOutcome);
        assert!(store
            .list_surface_leases()
            .unwrap()
            .iter()
            .all(|lease| lease.run_id != run_b.run_id));

        let isolated = ComputerUseService::new_simulator(
            Arc::new(SimulatorBackend::independently_isolated()),
            store.clone(),
        );
        let (run_c, token_c) = create_authorized_agent_run(&isolated, "uncertain-domain-c");
        observe_agent_run(&isolated, &run_c, &token_c, "uncertain-domain-c")
            .await
            .unwrap();
        assert!(store.list_surface_leases().unwrap().iter().any(|lease| {
            lease.run_id == run_c.run_id && lease.state == ComputerSurfaceLeaseState::Granted
        }));
    }

    #[tokio::test]
    async fn local_operator_reconciliation_quarantines_without_claiming_outcome() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let service =
            ComputerUseService::new_simulator(Arc::new(SimulatorBackend::new()), store.clone());
        let (run, token) = create_authorized_agent_run(&service, "reconcile");
        observe_agent_run(&service, &run, &token, "reconcile")
            .await
            .unwrap();
        let payload_sha256 = crate::orchestration::hash_payload(&json!({
            "action": "reconcile"
        }));
        let prepared = match store
            .acquire_agent_surface_dispatch(
                &run.run_id,
                "reconcile-dispatch",
                &payload_sha256,
                Utc::now(),
            )
            .unwrap()
        {
            ComputerDispatchClaim::Perform(lease) => lease,
            other => panic!("expected Perform, got {other:?}"),
        };
        let dispatch_key = dispatch_id(&prepared).unwrap().to_string();
        let injected = store
            .mark_surface_dispatch_injected(
                &prepared.lease_id,
                prepared.revision,
                &dispatch_key,
                Utc::now(),
            )
            .unwrap();
        let uncertain = store
            .fail_surface_dispatch(
                &injected.lease_id,
                &dispatch_key,
                ComputerErrorCode::Internal,
                Utc::now(),
            )
            .unwrap();
        assert_eq!(uncertain.state, ComputerSurfaceLeaseState::Uncertain);
        let handles = service
            .uncertain_surface_lease(&run.run_id)
            .unwrap()
            .expect("uncertain lease handles");
        assert_eq!(handles.lease_id, uncertain.lease_id);
        assert_eq!(handles.expected_revision, uncertain.revision);
        assert_eq!(handles.surface_id, uncertain.surface.surface_id);
        assert_eq!(handles.incarnation, uncertain.surface.incarnation);

        let operator = caller(&run, &service);
        service
            .reconcile_uncertain_surface_lease(
                "reconcile-uncertain-1",
                &operator,
                &uncertain.lease_id,
                uncertain.revision,
                &uncertain.surface.surface_id,
                &uncertain.surface.incarnation,
                "operator verified the exact surface is clear",
            )
            .unwrap();
        let reconciled = store
            .load_surface_lease(&uncertain.lease_id)
            .unwrap()
            .expect("reconciled lease");
        assert_eq!(reconciled.state, ComputerSurfaceLeaseState::Quarantined);
        assert_eq!(
            reconciled.dispatch.as_ref().map(|dispatch| dispatch.state),
            Some(ComputerDispatchState::Uncertain)
        );

        service
            .reconcile_uncertain_surface_lease(
                "reconcile-uncertain-1",
                &operator,
                &uncertain.lease_id,
                uncertain.revision,
                &uncertain.surface.surface_id,
                &uncertain.surface.incarnation,
                "operator verified the exact surface is clear",
            )
            .unwrap();

        let (next_run, next_token) = create_authorized_agent_run(&service, "reconcile-next");
        observe_agent_run(&service, &next_run, &next_token, "reconcile-next")
            .await
            .unwrap();
        assert!(store.list_surface_leases().unwrap().iter().any(|lease| {
            lease.run_id == next_run.run_id && lease.state == ComputerSurfaceLeaseState::Granted
        }));
    }

    #[tokio::test]
    async fn lease_expiry_fences_known_not_injected_and_uncertain_dispatches() {
        let root = tempdir().unwrap();

        let before_service = ComputerUseService::new_simulator(
            Arc::new(SimulatorBackend::new()),
            ComputerStore::open(root.path().join("before-injection")).unwrap(),
        );
        let (before_run, before_token) =
            create_authorized_agent_run(&before_service, "expiry-before");
        observe_agent_run(&before_service, &before_run, &before_token, "expiry-before")
            .await
            .unwrap();
        let before_lease = before_service
            .store
            .list_surface_leases()
            .unwrap()
            .remove(0);
        let before_error = before_service
            .store
            .prepare_surface_dispatch(
                &before_lease.lease_id,
                before_lease.revision,
                "expiry-before-dispatch",
                &"a".repeat(64),
                before_lease.expires_at + Duration::milliseconds(1),
            )
            .unwrap_err();
        assert_eq!(before_error.code, ComputerErrorCode::PermissionRevoked);
        let before_recovered = before_service
            .store
            .load_surface_lease(&before_lease.lease_id)
            .unwrap()
            .unwrap();
        assert_eq!(before_recovered.state, ComputerSurfaceLeaseState::Revoked);
        assert!(before_recovered.dispatch.is_none());

        let after_service = ComputerUseService::new_simulator(
            Arc::new(SimulatorBackend::new()),
            ComputerStore::open(root.path().join("after-injection")).unwrap(),
        );
        let (after_run, after_token) = create_authorized_agent_run(&after_service, "expiry-after");
        observe_agent_run(&after_service, &after_run, &after_token, "expiry-after")
            .await
            .unwrap();
        let after_lease = after_service.store.list_surface_leases().unwrap().remove(0);
        let after_payload = "b".repeat(64);
        let after_prepared = match after_service
            .store
            .prepare_surface_dispatch(
                &after_lease.lease_id,
                after_lease.revision,
                "expiry-after-dispatch",
                &after_payload,
                after_lease.updated_at,
            )
            .unwrap()
        {
            ComputerDispatchClaim::Perform(lease) => lease,
            other => panic!("expected Perform, got {other:?}"),
        };
        let after_injected = after_service
            .store
            .mark_surface_dispatch_injected(
                &after_prepared.lease_id,
                after_prepared.revision,
                "expiry-after-dispatch",
                after_prepared.updated_at,
            )
            .unwrap();
        assert_eq!(
            after_injected.dispatch.as_ref().unwrap().state,
            ComputerDispatchState::Injected
        );
        let reassignment_error = after_service
            .store
            .grant_next_surface_lease(
                &after_run.surface,
                after_injected.expires_at + Duration::milliseconds(1),
            )
            .unwrap_err();
        assert_eq!(reassignment_error.code, ComputerErrorCode::UncertainOutcome);
        let after_recovered = after_service
            .store
            .load_surface_lease(&after_lease.lease_id)
            .unwrap()
            .unwrap();
        assert_eq!(after_recovered.state, ComputerSurfaceLeaseState::Uncertain);
        assert_eq!(
            after_recovered.dispatch.as_ref().unwrap().state,
            ComputerDispatchState::Uncertain
        );
    }

    #[tokio::test]
    async fn corrupted_surface_lease_fails_closed_without_rewriting_records() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("computer-use");
        let (lease_path, run_path) = {
            let service = ComputerUseService::new_simulator(
                Arc::new(SimulatorBackend::new()),
                ComputerStore::open(&root).unwrap(),
            );
            let (run, token) = create_authorized_agent_run(&service, "corrupt-lease");
            observe_agent_run(&service, &run, &token, "corrupt-lease")
                .await
                .unwrap();
            let _lease = service.store.list_surface_leases().unwrap().remove(0);
            (
                std::fs::read_dir(root.join("surface-leases"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path(),
                std::fs::read_dir(root.join("runs"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path(),
            )
        };
        let run_before = std::fs::read(&run_path).unwrap();
        let mut lease_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&lease_path).unwrap()).unwrap();
        lease_value
            .as_object_mut()
            .unwrap()
            .insert("futureAuthority".into(), json!(true));
        std::fs::write(
            &lease_path,
            serde_json::to_vec_pretty(&lease_value).unwrap(),
        )
        .unwrap();
        let corrupt_before = std::fs::read(&lease_path).unwrap();

        assert!(ComputerStore::open(&root).is_err());
        assert_eq!(std::fs::read(&lease_path).unwrap(), corrupt_before);
        assert_eq!(std::fs::read(&run_path).unwrap(), run_before);
    }

    #[tokio::test]
    async fn backend_cannot_replace_the_host_minted_observation_identity() {
        let dir = tempdir().unwrap();
        let service = trusted_fixture_service(
            Arc::new(MismatchedObservationBackend::default()),
            ComputerStore::open(dir.path()).unwrap(),
        );
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-host-id",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-host-id",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();

        let error = service
            .observe(
                "observe-host-id",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::BackendFailure);

        let stored = service.get_run(&run.run_id).unwrap().unwrap();
        let projection = service
            .project_session_run(owner, &run.run_id, Utc::now())
            .unwrap();
        let events = service
            .session_run_events(owner, &run.run_id, None, 100)
            .unwrap();
        for encoded in [
            serde_json::to_string(&stored).unwrap(),
            serde_json::to_string(&projection).unwrap(),
            serde_json::to_string(&events).unwrap(),
        ] {
            assert!(!encoded.contains("PRIVATE_BACKEND_OBSERVATION_ID"));
        }
    }

    #[tokio::test]
    async fn simulator_run_is_durable_bounded_and_replay_safe() {
        let (backend, service) = service();
        let run = service
            .create_run(
                "create-1",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-1",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-1",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let after_observe = service.get_run(&run.run_id).unwrap().unwrap();
        let name_id = format!("{}-name", observation.observation_id);
        let outcome = service
            .act(
                "act-1",
                &caller(&run, &service),
                &run.run_id,
                after_observe.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: name_id,
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        let replay = service
            .act(
                "act-1",
                &caller(&run, &service),
                &run.run_id,
                after_observe.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome, replay);

        let after_name = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(after_name.action_count, 1);
        let observation = service
            .observe(
                "observe-2",
                &caller(&run, &service),
                &run.run_id,
                after_name.version,
            )
            .await
            .unwrap();
        let after_observe = service.get_run(&run.run_id).unwrap().unwrap();
        service
            .act(
                "act-2",
                &caller(&run, &service),
                &run.run_id,
                after_observe.version,
                &observation.observation_id,
                ComputerAction::Invoke {
                    element_id: format!("{}-submit", observation.observation_id),
                },
            )
            .await
            .unwrap();
        assert!(backend.submitted());
        assert_eq!(
            service.get_run(&run.run_id).unwrap().unwrap().action_count,
            2
        );
    }

    #[tokio::test]
    async fn conflicting_request_id_never_executes_a_second_action() {
        let (backend, service) = service();
        let run = service
            .create_run(
                "create-conflict",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-conflict",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-conflict",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let first = ComputerAction::SetValue {
            element_id: format!("{}-name", observation.observation_id),
            text: "Ada".into(),
        };
        service
            .act(
                "same-request",
                &caller(&run, &service),
                &run.run_id,
                current.version,
                &observation.observation_id,
                first,
            )
            .await
            .unwrap();
        let error = service
            .act(
                "same-request",
                &caller(&run, &service),
                &run.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::Invoke {
                    element_id: format!("{}-submit", observation.observation_id),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Conflict);
        assert!(!backend.submitted());
    }

    #[tokio::test]
    async fn pause_revokes_authority_and_requires_new_grant() {
        let (_backend, service) = service();
        let run = service
            .create_run(
                "create-pause",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-pause",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let paused = service
            .pause("pause-1", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert_eq!(paused.state, ComputerRunState::Paused);
        assert_eq!(
            paused.control_disposition,
            ComputerControlDisposition::Paused
        );
        assert!(paused.grant.unwrap().revoked_at.is_some());
        let error = service
            .observe(
                "observe-paused",
                &caller(&run, &service),
                &run.run_id,
                paused.version,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
    }

    #[tokio::test]
    async fn emergency_controls_accept_awaiting_authorization_without_client_version() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let operator = ComputerAuthorityToken::local_operator(owner).unwrap();
        let pause_run = service
            .create_run(
                "create-awaiting-pause",
                &operator,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        assert_eq!(
            service
                .pause("pause-awaiting", &operator, &pause_run.run_id)
                .await
                .unwrap()
                .control_disposition,
            ComputerControlDisposition::Paused
        );

        let takeover_operator = ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap();
        let takeover_run = service
            .create_run(
                "create-awaiting-takeover",
                &takeover_operator,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let taken_over = service
            .take_over(
                "takeover-awaiting",
                &takeover_operator,
                &takeover_run.run_id,
            )
            .await
            .unwrap();
        assert_eq!(taken_over.state, ComputerRunState::Paused);
        assert_eq!(
            taken_over.control_disposition,
            ComputerControlDisposition::OperatorTakeover
        );
    }

    #[tokio::test]
    async fn take_over_revokes_authority_and_is_distinct_in_audit() {
        let (_backend, service) = service();
        let run = service
            .create_run(
                "create-takeover",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-takeover",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let taken_over = service
            .take_over("takeover-1", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();

        assert_eq!(taken_over.state, ComputerRunState::Paused);
        assert_eq!(
            taken_over.control_disposition,
            ComputerControlDisposition::OperatorTakeover
        );
        assert!(taken_over.grant.unwrap().revoked_at.is_some());
        assert!(taken_over.audit.iter().any(|entry| {
            entry.operation == "take_over" && entry.disposition == "operator_control"
        }));
    }

    #[tokio::test]
    async fn operator_takeover_is_an_absorbing_control_fence() {
        let (_backend, service) = service();
        let run = service
            .create_run(
                "create-takeover-fence",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-takeover-fence",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let taken_over = service
            .take_over("takeover-fence", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();

        let error = service
            .authorize(
                "stale-authorize-after-takeover",
                &caller(&run, &service),
                &run.run_id,
                taken_over.version,
                grant(&taken_over),
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            persisted.control_disposition,
            ComputerControlDisposition::OperatorTakeover
        );
        assert_eq!(persisted.state, ComputerRunState::Paused);
        assert!(persisted.control_epoch > run.control_epoch);

        let error = service
            .pause("pause-after-takeover", &caller(&run, &service), &run.run_id)
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            persisted.control_disposition,
            ComputerControlDisposition::OperatorTakeover
        );
        assert_eq!(persisted.version, taken_over.version);
    }

    #[tokio::test]
    async fn denied_action_is_audited_without_storing_entered_text() {
        let (_backend, service) = service();
        let run = service
            .create_run(
                "create-denied",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let mut semantic_only = grant(&run);
        semantic_only.action_classes = BTreeSet::from([ActionClass::Semantic]);
        let run = service
            .authorize(
                "grant-denied",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                semantic_only,
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-denied",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let error = service
            .act(
                "deny-action",
                &caller(&run, &service),
                &run.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "NEVER_STORE_THIS_TEXT".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);

        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        let denial = persisted.audit.last().unwrap();
        assert_eq!(denial.operation, "act");
        assert_eq!(denial.disposition, "denied");
        assert_eq!(denial.action_class, Some(ActionClass::TextEntry));
        assert_eq!(denial.error_code, Some(ComputerErrorCode::ForbiddenAction));
        assert!(!serde_json::to_string(&persisted)
            .unwrap()
            .contains("NEVER_STORE_THIS_TEXT"));
        let receipt_contents = std::fs::read_dir(service.store.root().join("receipts"))
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect::<String>();
        assert!(!receipt_contents.contains("NEVER_STORE_THIS_TEXT"));
    }

    #[tokio::test]
    async fn oversized_action_is_rejected_before_claiming_a_receipt() {
        let (_backend, service) = service();
        let error = service
            .act(
                "oversized-action",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                "missing-run",
                1,
                "missing-observation",
                ComputerAction::SetValue {
                    element_id: "field".into(),
                    text: "x".repeat(16 * 1024 + 1),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::LimitReached);
        assert_eq!(
            std::fs::read_dir(service.store.root().join("receipts"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn evidence_limit_is_committed_before_returning_the_error() {
        let dir = tempdir().unwrap();
        let service = trusted_fixture_service(
            Arc::new(EvidenceBackend::default()),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let limits = ComputerUseLimits {
            max_evidence_bytes: 1,
            ..Default::default()
        };
        let run = service
            .create_run(
                "create-evidence-limit",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                limits,
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-evidence-limit",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let error = service
            .observe(
                "observe-evidence-limit",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::LimitReached);
        let terminal = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(terminal.state, ComputerRunState::LimitReached);
        assert!(terminal.current_observation.is_none());
        assert!(terminal.grant.unwrap().revoked_at.is_some());
    }

    #[tokio::test]
    async fn evidence_read_requires_current_asset_and_validates_backend_bytes() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(EvidenceBackend::default());
        let service = trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let run = service
            .create_run(
                "create-evidence-read",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-evidence-read",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-evidence-read",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let evidence = observation.screenshot.unwrap();

        assert_eq!(
            service
                .read_current_evidence(&caller(&run, &service), &run.run_id, &evidence.asset_id)
                .await
                .unwrap(),
            b"ok"
        );
        assert_eq!(
            service
                .read_current_evidence(&caller(&run, &service), &run.run_id, "not-current")
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );

        *backend.bytes.lock() = b"no".to_vec();
        assert_eq!(
            service
                .read_current_evidence(&caller(&run, &service), &run.run_id, &evidence.asset_id)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::BackendFailure
        );
    }

    #[tokio::test]
    async fn duration_limit_is_committed_and_revokes_authority() {
        let (_backend, service) = service();
        let run = service
            .create_run(
                "create-duration-limit",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-duration-limit",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        service
            .store
            .update_run(&run.run_id, |stored| {
                stored.started_at = Some(Utc::now() - Duration::minutes(16));
                Ok(())
            })
            .unwrap();
        let error = service
            .observe(
                "observe-duration-limit",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::LimitReached);
        let terminal = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(terminal.state, ComputerRunState::LimitReached);
        assert!(terminal.grant.unwrap().revoked_at.is_some());
    }

    #[tokio::test]
    async fn exhausting_a_grant_pauses_and_revokes_the_run() {
        let (_backend, service) = service();
        let run = service
            .create_run(
                "create-one-use",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let mut one_use = grant(&run);
        one_use.uses_remaining = Some(1);
        let run = service
            .authorize(
                "grant-one-use",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                one_use,
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-one-use",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        service
            .act(
                "act-one-use",
                &caller(&run, &service),
                &run.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        let paused = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(paused.state, ComputerRunState::Paused);
        assert_eq!(
            paused.control_disposition,
            ComputerControlDisposition::Paused
        );
        assert!(paused.current_observation.is_none());
        assert!(paused.grant.unwrap().revoked_at.is_some());
    }

    #[tokio::test]
    async fn concurrent_actions_execute_the_backend_at_most_once() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let run = service
            .create_run(
                "create-race",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let expected_version = current.version;
        let first_service = service.clone();
        let first_run_id = run.run_id.clone();
        let first_observation = observation.clone();
        let first_caller = caller(&run, &service);
        let first = tokio::spawn(async move {
            first_service
                .act(
                    "act-race-first",
                    &first_caller,
                    &first_run_id,
                    expected_version,
                    &first_observation.observation_id,
                    ComputerAction::SetValue {
                        element_id: format!("{}-name", first_observation.observation_id),
                        text: "Ada".into(),
                    },
                )
                .await
        });
        backend.action_entered.notified().await;

        let error = service
            .act(
                "act-race-second",
                &caller(&run, &service),
                &run.run_id,
                expected_version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Grace".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Conflict);
        assert_eq!(backend.action_calls.load(Ordering::SeqCst), 1);

        backend.release_action.notify_one();
        first.await.unwrap().unwrap();
        assert_eq!(backend.action_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_wins_over_an_inflight_action_completion() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let run = service
            .create_run(
                "create-cancel-race",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-cancel-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-cancel-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let action_service = service.clone();
        let action_run_id = run.run_id.clone();
        let action_observation = observation.clone();
        let action_caller = caller(&run, &service);
        let action = tokio::spawn(async move {
            action_service
                .act(
                    "act-cancel-race",
                    &action_caller,
                    &action_run_id,
                    current.version,
                    &action_observation.observation_id,
                    ComputerAction::SetValue {
                        element_id: format!("{}-name", action_observation.observation_id),
                        text: "Ada".into(),
                    },
                )
                .await
        });
        backend.action_entered.notified().await;

        let cancelled = service
            .cancel("cancel-race", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert_eq!(cancelled.state, ComputerRunState::Cancelled);
        assert_eq!(
            cancelled.control_disposition,
            ComputerControlDisposition::Stopped
        );
        let error = action.await.unwrap().unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(persisted.state, ComputerRunState::Cancelled);
        assert_eq!(persisted.action_count, 0);
        assert!(persisted.current_observation.is_none());
        assert_eq!(
            persisted.last_error.as_ref().map(|error| error.code),
            Some(ComputerErrorCode::UncertainOutcome)
        );
        assert!(persisted
            .audit
            .iter()
            .any(|entry| entry.disposition == "uncertain_outcome"));
    }

    #[tokio::test]
    async fn pause_uses_current_durable_version_and_wins_inflight_action_race() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let run = service
            .create_run(
                "create-pause-race",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-pause-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-pause-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let before_dispatch = service.get_run(&run.run_id).unwrap().unwrap();
        let before_dispatch_version = before_dispatch.version;
        let action_service = service.clone();
        let action_run_id = run.run_id.clone();
        let action_observation = observation.clone();
        let action_caller = caller(&run, &service);
        let action = tokio::spawn(async move {
            action_service
                .act(
                    "act-pause-race",
                    &action_caller,
                    &action_run_id,
                    before_dispatch_version,
                    &action_observation.observation_id,
                    ComputerAction::SetValue {
                        element_id: format!("{}-name", action_observation.observation_id),
                        text: "Ada".into(),
                    },
                )
                .await
        });
        backend.action_entered.notified().await;
        let acting = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(acting.state, ComputerRunState::Acting);
        assert!(acting.version > before_dispatch_version);

        let paused = service
            .pause("pause-race", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert_eq!(paused.state, ComputerRunState::Paused);
        assert_eq!(
            paused.control_disposition,
            ComputerControlDisposition::Paused
        );
        assert!(paused
            .grant
            .as_ref()
            .is_some_and(|grant| grant.revoked_at.is_some()));
        assert_eq!(
            action.await.unwrap().unwrap_err().code,
            ComputerErrorCode::UncertainOutcome
        );
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(persisted.state, ComputerRunState::Paused);
        assert_eq!(
            persisted.control_disposition,
            ComputerControlDisposition::Paused
        );
        assert!(persisted
            .audit
            .iter()
            .any(|entry| entry.operation == "pause" && entry.disposition == "paused"));
    }

    #[tokio::test]
    async fn takeover_uses_current_durable_version_and_wins_inflight_action_race() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let run = service
            .create_run(
                "create-takeover-race",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-takeover-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-takeover-race",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let before_dispatch = service.get_run(&run.run_id).unwrap().unwrap();
        let before_dispatch_version = before_dispatch.version;
        let action_service = service.clone();
        let action_run_id = run.run_id.clone();
        let action_observation = observation.clone();
        let action_caller = caller(&run, &service);
        let action = tokio::spawn(async move {
            action_service
                .act(
                    "act-takeover-race",
                    &action_caller,
                    &action_run_id,
                    before_dispatch_version,
                    &action_observation.observation_id,
                    ComputerAction::SetValue {
                        element_id: format!("{}-name", action_observation.observation_id),
                        text: "Ada".into(),
                    },
                )
                .await
        });
        backend.action_entered.notified().await;
        let acting = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(acting.state, ComputerRunState::Acting);
        assert!(acting.version > before_dispatch_version);

        let taken_over = service
            .take_over("takeover-race", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert_eq!(taken_over.state, ComputerRunState::Paused);
        assert_eq!(
            taken_over.control_disposition,
            ComputerControlDisposition::OperatorTakeover
        );
        assert!(taken_over
            .grant
            .as_ref()
            .is_some_and(|grant| grant.revoked_at.is_some()));
        assert_eq!(
            action.await.unwrap().unwrap_err().code,
            ComputerErrorCode::UncertainOutcome
        );
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(persisted.state, ComputerRunState::Paused);
        assert_eq!(
            persisted.control_disposition,
            ComputerControlDisposition::OperatorTakeover
        );
        assert!(persisted.audit.iter().any(|entry| {
            entry.operation == "take_over" && entry.disposition == "operator_control"
        }));
    }

    #[tokio::test]
    async fn scoped_reads_refuse_another_session_and_never_confirm_run_existence() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let intruder = Uuid::new_v4();
        let run = service
            .create_run(
                "create-scope",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let now = Utc::now();

        let cross_session = service
            .project_session_run(intruder, &run.run_id, now)
            .unwrap_err();
        let unknown_run = service
            .project_session_run(intruder, "no-such-run", now)
            .unwrap_err();
        let unknown_to_owner = service
            .project_session_run(owner, "no-such-run", now)
            .unwrap_err();

        assert_eq!(cross_session.code, ComputerErrorCode::Unauthorized);
        // A real run belonging to someone else must be indistinguishable from a
        // run that does not exist, or the read becomes an existence oracle.
        assert_eq!(cross_session, unknown_run);
        assert_eq!(cross_session, unknown_to_owner);

        assert_eq!(
            service
                .session_run_events(intruder, &run.run_id, None, 10)
                .unwrap_err(),
            cross_session
        );
        assert!(service.project_session_run(owner, &run.run_id, now).is_ok());
    }

    #[tokio::test]
    async fn traversal_shaped_run_ids_fail_closed_as_unauthorized() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        for probe in ["../escape", "runs/../../etc/passwd", "", "  "] {
            let error = service
                .project_session_run(owner, probe, Utc::now())
                .unwrap_err();
            assert_eq!(
                error.code,
                ComputerErrorCode::Unauthorized,
                "run id {probe:?} must not leak a distinct validation error"
            );
        }
    }

    #[tokio::test]
    async fn listing_and_capacity_are_scoped_to_the_owning_session() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mine = service
            .create_run(
                "create-mine",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        service
            .create_run(
                "create-theirs",
                &ComputerAuthorityToken::local_operator(other).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();

        let listed = service
            .list_session_run_projections(owner, Utc::now())
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].run_id, mine.run_id);
        assert_eq!(listed[0].owner_session_id, owner);

        let capacity = service.session_capacity(owner).unwrap();
        assert_eq!(capacity.stored_runs, 2);
        assert_eq!(capacity.session_runs, 1);
        assert_eq!(capacity.session_active_runs, 1);
        assert_eq!(
            capacity.max_run_records,
            ComputerStore::MAX_RUN_RECORDS as u32
        );

        // Cancelling the owner's run must not change the other session's view.
        service
            .cancel("cancel-mine", &caller(&mine, &service), &mine.run_id)
            .await
            .unwrap();
        assert_eq!(
            service.session_capacity(owner).unwrap().session_active_runs,
            0
        );
        assert_eq!(
            service.session_capacity(other).unwrap().session_active_runs,
            1
        );
    }

    #[tokio::test]
    async fn session_scoped_read_matches_direct_projection() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-parity",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-parity",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        service
            .observe(
                "observe-parity",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();

        let now = Utc::now();
        // The desktop path projects the durable record it already holds; the
        // session-scoped local read reloads it. Both must serialize identically.
        // Coordinator reads take ComputerReadBinding on ComputerRunReads.
        let gui = crate::computer_use::project_run_at(
            &service.get_run(&run.run_id).unwrap().unwrap(),
            now,
        );
        let session = service
            .project_session_run(owner, &run.run_id, now)
            .unwrap();
        assert_eq!(
            serde_json::to_string(&gui).unwrap(),
            serde_json::to_string(&session).unwrap()
        );
        assert_eq!(
            session,
            service.list_session_run_projections(owner, now).unwrap()[0]
        );
    }

    #[tokio::test]
    async fn observation_contents_never_reach_the_scoped_projection() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-redaction",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-redaction",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-redaction",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        assert!(
            !observation.elements.is_empty(),
            "fixture must observe elements for this assertion to mean anything"
        );

        let projection = service
            .project_session_run(owner, &run.run_id, Utc::now())
            .unwrap();
        let encoded = serde_json::to_value(&projection).unwrap();

        // Compare against string *values* only. A raw substring scan over the
        // whole document also matches JSON keys, so a short label such as
        // "Name" would false-positive against the `displayName` key.
        fn string_values(value: &serde_json::Value, out: &mut Vec<String>) {
            match value {
                serde_json::Value::String(text) => out.push(text.clone()),
                serde_json::Value::Array(items) => {
                    items.iter().for_each(|item| string_values(item, out))
                }
                serde_json::Value::Object(map) => {
                    map.values().for_each(|item| string_values(item, out))
                }
                _ => {}
            }
        }
        let mut values = Vec::new();
        string_values(&encoded, &mut values);

        for element in &observation.elements {
            for projected in &values {
                assert!(
                    !projected.contains(&element.element_id),
                    "element ids are observation-scoped capabilities and must not be projected"
                );
                if let Some(label) = &element.label {
                    assert_ne!(projected, label, "observed labels must not be projected");
                }
                if let Some(value) = &element.value {
                    assert_ne!(projected, value, "observed values must not be projected");
                }
            }
        }

        // Pin the exact observation key set so a future field addition cannot
        // quietly widen what a coordinator observes.
        let observation_keys: BTreeSet<&str> = encoded["observation"]
            .as_object()
            .expect("observation is projected")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            observation_keys,
            BTreeSet::from([
                "observationId",
                "sequence",
                "capturedAt",
                "elementCount",
                "elementsTruncated",
                "sensitivity",
                "hasScreenshot",
                "screenshotRedacted",
                "stale",
                "surfaceId",
                "frameEpoch",
            ])
        );
        assert_eq!(
            projection.observation.as_ref().unwrap().element_count,
            observation.elements.len() as u32
        );

        if let Some(last_outcome) = encoded
            .get("lastOutcome")
            .and_then(|value| value.as_object())
        {
            let keys: BTreeSet<&str> = last_outcome.keys().map(String::as_str).collect();
            assert_eq!(keys, BTreeSet::from(["expectedPostconditionMet"]));
        }
        if let Some(last_error) = encoded.get("lastError").and_then(|value| value.as_object()) {
            let keys: BTreeSet<&str> = last_error.keys().map(String::as_str).collect();
            assert_eq!(keys, BTreeSet::from(["code"]));
        }
    }

    #[tokio::test]
    async fn restart_projects_interrupted_control_state_and_keeps_events_readable() {
        let dir = tempdir().unwrap().keep();
        let owner = Uuid::new_v4();
        let run_id;
        {
            let service = ComputerUseService::new_simulator(
                Arc::new(SimulatorBackend::new()),
                ComputerStore::open(dir.join("computer-use")).unwrap(),
            );
            let run = service
                .create_run(
                    "create-restart",
                    &ComputerAuthorityToken::local_operator(owner).unwrap(),
                    None,
                    SimulatorBackend::demo_target(),
                    Default::default(),
                )
                .unwrap();
            run_id = run.run_id.clone();
            service
                .authorize(
                    "grant-restart",
                    &caller(&run, &service),
                    &run.run_id,
                    run.version,
                    grant(&run),
                )
                .unwrap();
        }

        let service = ComputerUseService::new_simulator(
            Arc::new(SimulatorBackend::new()),
            ComputerStore::open(dir.join("computer-use")).unwrap(),
        );
        let recovered = service
            .project_session_run(owner, &run_id, Utc::now())
            .unwrap();
        assert_eq!(recovered.state, ComputerRunState::Interrupted);
        assert_eq!(
            recovered.control_disposition,
            ComputerControlDisposition::Interrupted
        );
        assert!(recovered.terminal);
        assert!(!recovered.agent_active);
        assert!(recovered.control_epoch > 0);
        assert!(
            recovered.grant.is_none(),
            "authority must not survive restart"
        );
        assert!(recovered.observation.is_none());
        assert!(
            recovered.last_outcome.is_none(),
            "restart must not keep a leaky last_outcome"
        );

        // Durable events survive the restart and stay replayable from the start.
        let page = service
            .session_run_events(owner, &run_id, None, 500)
            .unwrap();
        assert!(!page.cursor_expired);
        assert!(page
            .entries
            .iter()
            .any(|entry| entry.operation == "create_run"));
        let range = recovered.event_range.expect("recovered run has events");
        assert_eq!(page.range, Some(range));

        // Replaying from the final sequence is a valid empty tail, not a gap.
        let tail = service
            .session_run_events(owner, &run_id, Some(range.end_seq), 500)
            .unwrap();
        assert!(tail.entries.is_empty());
        assert!(!tail.cursor_expired);
        assert_eq!(tail.next_cursor, None);
    }

    #[tokio::test]
    async fn principal_mismatch_denies_service_mutations_before_dispatch() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-principal",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let intruder = ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap();
        assert_eq!(
            service
                .authorize(
                    "grant-intruder",
                    &intruder,
                    &run.run_id,
                    run.version,
                    grant(&run),
                )
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );

        let run = service
            .authorize(
                "grant-ok",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let other = ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap();
        assert_eq!(
            service
                .observe("observe-intruder", &other, &run.run_id, run.version)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(
            service
                .take_over("takeover-intruder", &other, &run.run_id)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
        assert_eq!(
            service
                .read_current_evidence(&other, &run.run_id, "asset")
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[tokio::test]
    async fn background_fixture_cannot_activate_and_isolated_fixture_can_pointer() {
        let dir = tempdir().unwrap();
        let background = ComputerUseService::new_simulator(
            Arc::new(SimulatorBackend::measured_background_safe()),
            ComputerStore::open(dir.path().join("bg")).unwrap(),
        );
        let owner = Uuid::new_v4();
        let run = background
            .create_run(
                "bg-create",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        assert_eq!(
            run.capability_proof.tier(),
            crate::computer_use::ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
        );
        let run = background
            .authorize(
                "bg-grant",
                &caller(&run, &background),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = background
            .observe(
                "bg-observe",
                &caller(&run, &background),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let current = background.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            background
                .act(
                    "bg-activate",
                    &caller(&current, &background),
                    &current.run_id,
                    current.version,
                    &observation.observation_id,
                    ComputerAction::ActivateTarget,
                )
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::ForbiddenAction
        );

        let isolated = ComputerUseService::new_simulator(
            Arc::new(SimulatorBackend::independently_isolated()),
            ComputerStore::open(dir.path().join("iso")).unwrap(),
        );
        let run = isolated
            .create_run(
                "iso-create",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        assert!(run.capability_proof.is_simulator_only_isolation());
        assert_eq!(
            run.capability_proof.isolated_surface().as_ref(),
            Some(&run.surface),
            "isolated proof must bind the host-interned surface, not backend-supplied dummy ids"
        );
        let now = Utc::now();
        let grant = ActionGrant::for_run(
            &run,
            BTreeSet::from([
                ActionClass::Semantic,
                ActionClass::TextEntry,
                ActionClass::PointerFallback,
                ActionClass::KeyChord,
            ]),
            now,
            now + Duration::minutes(5),
            Some(8),
        );
        let run = isolated
            .authorize(
                "iso-grant",
                &caller(&run, &isolated),
                &run.run_id,
                run.version,
                grant,
            )
            .unwrap();
        let observation = isolated
            .observe(
                "iso-observe",
                &caller(&run, &isolated),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let current = isolated.get_run(&run.run_id).unwrap().unwrap();
        isolated
            .act(
                "iso-pointer",
                &caller(&current, &isolated),
                &current.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::PointerClick {
                    x: 10.0,
                    y: 10.0,
                    button: crate::computer_use::PointerButton::Primary,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shared_surface_older_tick_is_stale_and_does_not_dispatch() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let service = trusted_fixture_service(backend.clone(), store);
        let owner = Uuid::new_v4();
        let run_a = service
            .create_run(
                "create-a",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run_a = service
            .authorize(
                "grant-a",
                &caller(&run_a, &service),
                &run_a.run_id,
                run_a.version,
                grant(&run_a),
            )
            .unwrap();
        let observation_a = service
            .observe(
                "observe-a",
                &caller(&run_a, &service),
                &run_a.run_id,
                run_a.version,
            )
            .await
            .unwrap();
        assert_eq!(observation_a.authority.freshness.tick, 1);

        let run_b = service
            .create_run(
                "create-b",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        assert_eq!(run_b.surface, run_a.surface);
        let run_b = service
            .authorize(
                "grant-b",
                &caller(&run_b, &service),
                &run_b.run_id,
                run_b.version,
                grant(&run_b),
            )
            .unwrap();
        let observation_b = service
            .observe(
                "observe-b",
                &caller(&run_b, &service),
                &run_b.run_id,
                run_b.version,
            )
            .await
            .unwrap();
        assert_eq!(observation_b.authority.freshness.tick, 2);
        assert_eq!(observation_b.authority.frame_epoch, 2);
        assert_ne!(
            observation_b.sequence, observation_b.authority.frame_epoch,
            "backend sequence is diagnostic and is not the host frame epoch"
        );

        let current_a = service.get_run(&run_a.run_id).unwrap().unwrap();
        let error = service
            .act(
                "act-stale-a",
                &caller(&current_a, &service),
                &current_a.run_id,
                current_a.version,
                &observation_a.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation_a.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
        assert_eq!(backend.action_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn two_services_share_host_surface_registry_for_one_domain() {
        let dir = tempdir().unwrap();
        let backend = Arc::new(SimulatorBackend::new());
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let first = ComputerUseService::new_simulator(backend.clone(), store.clone());
        let second = ComputerUseService::new_simulator(backend, store);
        let owner = Uuid::new_v4();
        let run_a = first
            .create_run(
                "shared-a",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run_b = second
            .create_run(
                "shared-b",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        assert_eq!(run_a.surface, run_b.surface);
        let run_a = first
            .authorize(
                "shared-grant-a",
                &caller(&run_a, &first),
                &run_a.run_id,
                run_a.version,
                grant(&run_a),
            )
            .unwrap();
        first
            .observe(
                "shared-obs-a",
                &caller(&run_a, &first),
                &run_a.run_id,
                run_a.version,
            )
            .await
            .unwrap();
        let run_b = second
            .authorize(
                "shared-grant-b",
                &caller(&run_b, &second),
                &run_b.run_id,
                run_b.version,
                grant(&run_b),
            )
            .unwrap();
        let observation_b = second
            .observe(
                "shared-obs-b",
                &caller(&run_b, &second),
                &run_b.run_id,
                run_b.version,
            )
            .await
            .unwrap();
        assert_eq!(observation_b.authority.freshness.tick, 2);
    }

    #[tokio::test]
    async fn cross_principal_replay_fails_closed_and_does_not_return_observation() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-replay",
                &ComputerAuthorityToken::local_operator(owner).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-replay",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe(
                "observe-shared-id",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let replayed = service
            .observe(
                "observe-shared-id",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        assert_eq!(replayed.observation_id, observation.observation_id);

        let intruder = ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap();
        assert_eq!(
            service
                .observe("observe-shared-id", &intruder, &run.run_id, run.version,)
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[derive(Debug)]
    struct CountingBackend {
        inner: SimulatorBackend,
        acts: AtomicUsize,
        cancels: AtomicUsize,
    }

    impl CountingBackend {
        fn new() -> Self {
            Self {
                inner: SimulatorBackend::new(),
                acts: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for CountingBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            self.inner.capabilities()
        }

        fn physical_input_domain(&self) -> crate::computer_use::PhysicalInputDomain {
            self.inner.physical_input_domain()
        }

        async fn observe(
            &self,
            run_id: &str,
            observation_id: &str,
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            self.inner
                .observe(run_id, observation_id, target, limits)
                .await
        }

        async fn act(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.inner.act(run_id, observation, action).await
        }

        async fn act_if_current(
            &self,
            run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.acts.fetch_add(1, Ordering::SeqCst);
            self.inner.act_if_current(run_id, observation, action).await
        }

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            self.inner.cancel(run_id).await
        }
    }

    #[derive(Debug)]
    struct UnprovenBackend;

    #[derive(Debug)]
    struct ForgedIsolatedBackend {
        claimed_domain: crate::computer_use::PhysicalInputDomain,
    }

    impl ForgedIsolatedBackend {
        fn new(domain: &str) -> Self {
            Self {
                claimed_domain: crate::computer_use::PhysicalInputDomain::attested(
                    "attacker", domain,
                )
                .expect("forged fixture domain is syntactically valid"),
            }
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for ForgedIsolatedBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            SimulatorBackend::independently_isolated().capabilities()
        }

        fn physical_input_domain(&self) -> crate::computer_use::PhysicalInputDomain {
            self.claimed_domain.clone()
        }

        async fn observe(
            &self,
            _run_id: &str,
            _observation_id: &str,
            _target: &ComputerTarget,
            _limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            panic!("an unattested backend must never receive observation dispatch")
        }

        async fn act(
            &self,
            _run_id: &str,
            _observation: &ComputerObservation,
            _action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            panic!("an unattested backend must never receive action dispatch")
        }

        async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
            panic!("an unattested backend must never receive cancellation dispatch")
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for UnprovenBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            ComputerCapabilities::unproven("unproven_fixture")
        }

        fn physical_input_domain(&self) -> crate::computer_use::PhysicalInputDomain {
            crate::computer_use::PhysicalInputDomain::attested("unproven", "fixture")
                .expect("unproven fixture domain")
        }

        async fn observe(
            &self,
            _run_id: &str,
            _observation_id: &str,
            _target: &ComputerTarget,
            _limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven backend must not observe",
            ))
        }

        async fn act(
            &self,
            _run_id: &str,
            _observation: &ComputerObservation,
            _action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven backend must not act",
            ))
        }

        async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn unproven_backend_cannot_create_or_observe() {
        let dir = tempdir().unwrap();
        let service = ComputerUseService::new(
            Arc::new(UnprovenBackend),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let error = service
            .create_run(
                "create-unproven",
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);
    }

    #[test]
    fn public_backend_cannot_self_attest_isolation_or_parallel_input_domains() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let first = ComputerUseService::new(
            Arc::new(ForgedIsolatedBackend::new("forged-domain-a")),
            store.clone(),
        );
        let second = ComputerUseService::new(
            Arc::new(ForgedIsolatedBackend::new("forged-domain-b")),
            store.clone(),
        );

        assert_eq!(
            first.capabilities().tier,
            crate::computer_use::ComputerCapabilityTier::Unproven
        );
        assert_eq!(
            second.capabilities().tier,
            crate::computer_use::ComputerCapabilityTier::Unproven
        );
        for (request_id, service) in [("forged-a", &first), ("forged-b", &second)] {
            assert_eq!(
                service
                    .create_run(
                        request_id,
                        &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                        None,
                        SimulatorBackend::demo_target(),
                        Default::default(),
                    )
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenAction
            );
        }
        assert!(store.list_runs().unwrap().is_empty());
    }

    async fn authorized_ready(
        service: &ComputerUseService,
        request_prefix: &str,
        uses: Option<u32>,
    ) -> (ComputerRun, ComputerObservation) {
        let run = service
            .create_run(
                &format!("{request_prefix}-create"),
                &ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let mut issued = grant(&run);
        if uses.is_some() {
            issued.uses_remaining = uses;
        }
        let run = service
            .authorize(
                &format!("{request_prefix}-grant"),
                &caller(&run, service),
                &run.run_id,
                run.version,
                issued,
            )
            .unwrap();
        let observation = service
            .observe(
                &format!("{request_prefix}-observe"),
                &caller(&run, service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        (service.get_run(&run.run_id).unwrap().unwrap(), observation)
    }

    fn rewrite_receipt(
        service: &ComputerUseService,
        request_id: &str,
        rewrite: impl FnOnce(&mut serde_json::Value),
    ) {
        let path = service.store.receipt_path(request_id).unwrap();
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        rewrite(&mut receipt);
        std::fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn same_shape_content_drift_does_not_mutate_the_backend() {
        let (backend, service) = service();
        let (run, observation) = authorized_ready(&service, "drift", None).await;
        backend.mutate_content_preserving_shape(&run.run_id);
        let error = service
            .act(
                "act-drift",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
        assert_eq!(backend.mutation_count(), 0);
        let stored = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(stored.action_count, 0);
    }

    #[tokio::test]
    async fn epoch_changing_exact_retries_return_the_original_typed_result_once() {
        let backend = Arc::new(CountingBackend::new());
        let dir = tempdir().unwrap().keep();
        let service = trusted_fixture_service(
            backend.clone(),
            ComputerStore::open(dir.join("computer-use")).unwrap(),
        );

        let (run, _) = authorized_ready(&service, "pause-table", None).await;
        let paused = service
            .pause("pause-once", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert!(paused.control_epoch > run.control_epoch);
        assert_eq!(backend.cancels.load(Ordering::SeqCst), 1);
        let replayed = service
            .pause("pause-once", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert_eq!(paused, replayed);
        assert_eq!(backend.cancels.load(Ordering::SeqCst), 1);

        let (run, _) = authorized_ready(&service, "takeover-table", None).await;
        let taken = service
            .take_over("takeover-once", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        let cancels = backend.cancels.load(Ordering::SeqCst);
        let replayed = service
            .take_over("takeover-once", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert_eq!(taken, replayed);
        assert_eq!(backend.cancels.load(Ordering::SeqCst), cancels);

        let (run, _) = authorized_ready(&service, "cancel-table", None).await;
        let cancelled = service
            .cancel("cancel-once", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        let cancels = backend.cancels.load(Ordering::SeqCst);
        let replayed = service
            .cancel("cancel-once", &caller(&run, &service), &run.run_id)
            .await
            .unwrap();
        assert_eq!(cancelled, replayed);
        assert_eq!(backend.cancels.load(Ordering::SeqCst), cancels);

        let (run, _) = authorized_ready(&service, "complete-table", None).await;
        let completed = service
            .complete(
                "complete-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .unwrap();
        let replayed = service
            .complete(
                "complete-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .unwrap();
        assert_eq!(completed, replayed);
        assert_eq!(completed.state, ComputerRunState::Completed);

        let (run, observation) = authorized_ready(&service, "act-table", Some(1)).await;
        let action = ComputerAction::SetValue {
            element_id: format!("{}-name", observation.observation_id),
            text: "Ada".into(),
        };
        let outcome = service
            .act(
                "act-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                &observation.observation_id,
                action.clone(),
            )
            .await
            .unwrap();
        assert_eq!(backend.acts.load(Ordering::SeqCst), 1);
        assert_eq!(backend.inner.mutation_count(), 1);
        let replayed = service
            .act(
                "act-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                &observation.observation_id,
                action,
            )
            .await
            .unwrap();
        assert_eq!(outcome, replayed);
        assert_eq!(backend.acts.load(Ordering::SeqCst), 1);
        assert_eq!(backend.inner.mutation_count(), 1);
        let stored = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(stored.action_count, 1);
        assert_eq!(stored.state, ComputerRunState::Paused);
    }

    #[tokio::test]
    async fn unauthorized_unique_request_ids_do_not_consume_receipt_capacity() {
        let (backend, service) = service();
        let (run, observation) = authorized_ready(&service, "capacity", None).await;
        let audit_len = run.audit.len();
        let stranger = ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap();
        let receipts = service.store.root().join("receipts");
        let existing = std::fs::read_dir(&receipts).unwrap().count();
        let pads = super::super::store::MAX_RECEIPTS
            .saturating_sub(1)
            .saturating_sub(existing);
        for index in 0..pads {
            std::fs::write(receipts.join(format!("pad-{index}.json")), b"{}").unwrap();
        }
        let error = service
            .act(
                "unique-unauth",
                &stranger,
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Unauthorized);
        assert!(!service
            .store
            .receipt_path("unique-unauth")
            .unwrap()
            .is_file());
        assert_eq!(
            service.get_run(&run.run_id).unwrap().unwrap().audit.len(),
            audit_len
        );
        assert_eq!(backend.mutation_count(), 0);

        let outcome = service
            .act(
                "unique-auth",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.expected_postcondition_met, Some(true));
        assert!(service.store.receipt_path("unique-auth").unwrap().is_file());
        assert_eq!(backend.mutation_count(), 1);
    }

    #[tokio::test]
    async fn behavioral_mismatch_matrix_never_calls_the_backend() {
        let (backend, service) = service();
        let (run, observation) = authorized_ready(&service, "matrix", Some(1)).await;
        let action = ComputerAction::SetValue {
            element_id: format!("{}-name", observation.observation_id),
            text: "Ada".into(),
        };
        service
            .act(
                "act-matrix",
                &caller(&run, &service),
                &run.run_id,
                run.version,
                &observation.observation_id,
                action.clone(),
            )
            .await
            .unwrap();
        assert_eq!(backend.mutation_count(), 1);

        let stranger = ComputerAuthorityToken::local_operator(Uuid::new_v4()).unwrap();
        let principal = service
            .act(
                "act-matrix",
                &stranger,
                &run.run_id,
                run.version,
                &observation.observation_id,
                action.clone(),
            )
            .await
            .unwrap_err();
        assert_eq!(principal.code, ComputerErrorCode::Unauthorized);
        assert_eq!(backend.mutation_count(), 1);

        let original = service.store.load_run(&run.run_id).unwrap().unwrap();
        let restore_binding = |receipt: &mut serde_json::Value| {
            receipt["callerOwnerSessionId"] = serde_json::json!(original.owner_session_id);
            receipt["surfaceId"] = serde_json::json!(original.surface.surface_id);
            receipt["incarnation"] = serde_json::json!(original.surface.incarnation);
            receipt["grantId"] = serde_json::json!(original.grant.as_ref().unwrap().grant_id);
            receipt["runId"] = serde_json::json!(original.run_id);
        };

        for (label, rewrite) in [
            (
                "principal",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["callerOwnerSessionId"] = serde_json::json!(Uuid::new_v4());
                }) as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "surface",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["surfaceId"] = serde_json::json!("surface-mismatch");
                }),
            ),
            (
                "incarnation",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["incarnation"] = serde_json::json!("incarnation-mismatch");
                }),
            ),
            (
                "grant",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["grantId"] = serde_json::json!("grant-mismatch");
                }),
            ),
            (
                "receipt-binding",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["runId"] = serde_json::json!("run-mismatch");
                }),
            ),
        ] {
            rewrite_receipt(&service, "act-matrix", |receipt| rewrite(receipt));
            let error = service
                .act(
                    "act-matrix",
                    &caller(&run, &service),
                    &run.run_id,
                    run.version,
                    &observation.observation_id,
                    action.clone(),
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ComputerErrorCode::Unauthorized,
                "{label} mismatch must remain denied"
            );
            assert_eq!(backend.mutation_count(), 1, "{label} must not dispatch");
            rewrite_receipt(&service, "act-matrix", restore_binding);
        }

        let committed_receipt: serde_json::Value = serde_json::from_slice(
            &std::fs::read(service.store.receipt_path("act-matrix").unwrap()).unwrap(),
        )
        .unwrap();
        for (label, rewrite) in [
            (
                "frame",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["frameEpoch"] = serde_json::json!(99);
                }) as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "authority-epoch",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["preAuthorityEpoch"] = serde_json::json!(99);
                }),
            ),
            (
                "control-epoch",
                Box::new(|receipt: &mut serde_json::Value| {
                    receipt["preControlEpoch"] = serde_json::json!(99);
                }),
            ),
        ] {
            rewrite_receipt(&service, "act-matrix", |receipt| rewrite(receipt));
            let replayed = service
                .act(
                    "act-matrix",
                    &caller(&run, &service),
                    &run.run_id,
                    run.version,
                    &observation.observation_id,
                    action.clone(),
                )
                .await
                .unwrap();
            assert_eq!(
                replayed.expected_postcondition_met,
                Some(true),
                "{label} exact retry must return the original typed result"
            );
            assert_eq!(backend.mutation_count(), 1, "{label} must not dispatch");
            rewrite_receipt(&service, "act-matrix", |receipt| {
                *receipt = committed_receipt.clone();
            });
        }
        assert_eq!(
            service.get_run(&run.run_id).unwrap().unwrap().action_count,
            1
        );
    }
}
