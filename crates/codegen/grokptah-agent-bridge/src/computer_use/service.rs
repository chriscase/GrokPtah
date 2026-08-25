use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::policy::ComputerPolicy;
use super::projection::{
    not_available, project_events, project_run_at, ComputerRunCapacity, ComputerRunEventPage,
    ComputerRunProjection,
};
use super::store::{ComputerStore, MutationClaim, MutationStamp};
use super::types::{
    validate_id, ActionGrant, ActionOutcome, ComputerAction, ComputerAuthorityToken,
    ComputerBackend, ComputerBackendAttestation, ComputerControlDisposition, ComputerError,
    ComputerErrorCode, ComputerObservation, ComputerResult, ComputerRun, ComputerRunState,
    ComputerTarget, ComputerUseLimits, ObservationAuthority,
};

pub struct ComputerUseService {
    backend: Arc<dyn ComputerBackend>,
    store: ComputerStore,
    policy: ComputerPolicy,
    backend_attestation: ComputerBackendAttestation,
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
        }
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
        self.require_attested_dispatch(&run)?;
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
            if capabilities.proof.backend_id() == crate::computer_use::MACOS_NATIVE_BACKEND_ID
                && !matches!(
                    capabilities.proof,
                    crate::computer_use::ComputerCapabilityProof::ForegroundSemantic { .. }
                )
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::ForbiddenAction,
                    "native macOS Computer Use can only advertise foreground-semantic capability",
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
        let result = self
            .store
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
            .and_then(|run| run.ok_or_else(unknown_run));
        if let Err(error) = &result {
            self.record_denial(run_id, "authorize", None, error);
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
            (Ok(_), Some(error)) => Err(error),
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
                                Ok(observation) => Ok(observation),
                                Err(error) => {
                                    self.fail_inflight(run_id, "observe", &error)?;
                                    Err(error)
                                }
                            },
                            Err(error) => {
                                self.fail_inflight(run_id, "observe", &error)?;
                                Err(error)
                            }
                        }
                    }
                    Err(error) => {
                        self.fail_inflight(run_id, "observe", &error)?;
                        Err(error)
                    }
                }
            }
            (Err(error), _) => {
                self.record_denial(run_id, "observe", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
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
            (Ok(_), Some(error)) => Err(error),
            (Ok(prepared), None) => {
                let observation = prepared
                    .current_observation
                    .clone()
                    .expect("prepared action has an observation");
                let control_epoch = prepared.control_epoch;
                let outcome = self
                    .backend
                    .act_if_current(run_id, &observation, &action)
                    .await;
                match outcome {
                    Ok(outcome) => {
                        self.commit_action(run_id, &action, &observation, control_epoch, outcome)
                    }
                    Err(error) => {
                        self.fail_inflight(run_id, "act", &error)?;
                        Err(error)
                    }
                }
            }
            (Err(error), _) => {
                self.record_denial(run_id, "act", Some(action.class()), &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    pub async fn pause(
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
            self.begin_mutation(request_id, "pause", &payload, caller, Some(&current))?
        {
            return replayed;
        }
        let paused = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
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

    /// Yields durable operator control. This is bookkeeping-safe takeover: it
    /// revokes grants, bumps epochs, and cancels later backend work. It is not
    /// physically preemptive once an action is already inside the native
    /// action gate.
    pub async fn take_over(
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
            self.begin_mutation(request_id, "take_over", &payload, caller, Some(&current))?
        {
            return replayed;
        }
        let taken_over = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                self.require_caller(run, caller)?;
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
            .update_run(run_id, |run| {
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
            _ => {
                let run = run.ok_or_else(unknown_run)?;
                self.require_caller(run, caller)?;
                if matches!(
                    operation,
                    "observe" | "act" | "cancel" | "pause" | "take_over"
                ) {
                    self.require_attested_dispatch(run)?;
                }
                Ok(())
            }
        }
    }

    /// Existing-run observe/act/cancel (and equivalent backend dispatch) may
    /// proceed only when this service's crate-owned attestation matches the
    /// run's stamped proof and interned physical domain.
    fn require_attested_dispatch(&self, run: &ComputerRun) -> ComputerResult<()> {
        self.backend_attestation
            .require_matching_run_proof(&run.capability_proof)?;
        let interned = self
            .store
            .intern_physical_domain(self.backend_attestation.physical_domain())?;
        if run.surface != interned.binding {
            return Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "backend physical domain does not match the computer run surface",
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
    use crate::computer_use::{
        macos_native_capability_proof, macos_native_physical_input_domain, ActionClass,
        ComputerCapabilities, ComputerCapabilityTier, EvidenceRef, SimulatorBackend,
        MACOS_NATIVE_BACKEND_ID,
    };

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
            .pause("pause-1", &caller(&run, &service), &run.run_id, run.version)
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
            .take_over(
                "takeover-1",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
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
            .take_over(
                "takeover-fence",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
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
            .pause(
                "pause-after-takeover",
                &caller(&run, &service),
                &run.run_id,
                taken_over.version,
            )
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
                .take_over("takeover-intruder", &other, &run.run_id, run.version)
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

    #[derive(Debug, Default)]
    struct UnprovenBackend {
        observes: AtomicUsize,
        acts: AtomicUsize,
        cancels: AtomicUsize,
    }

    #[derive(Debug)]
    struct ForgedIsolatedBackend {
        claimed_domain: crate::computer_use::PhysicalInputDomain,
        observes: AtomicUsize,
        acts: AtomicUsize,
        cancels: AtomicUsize,
    }

    impl ForgedIsolatedBackend {
        fn new(domain: &str) -> Self {
            Self {
                claimed_domain: crate::computer_use::PhysicalInputDomain::attested(
                    "attacker", domain,
                )
                .expect("forged fixture domain is syntactically valid"),
                observes: AtomicUsize::new(0),
                acts: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
            }
        }
    }

    #[derive(Debug)]
    struct NativeForegroundFixture {
        inner: SimulatorBackend,
        observes: AtomicUsize,
        acts: AtomicUsize,
        cancels: AtomicUsize,
    }

    impl NativeForegroundFixture {
        fn new() -> Self {
            Self {
                inner: SimulatorBackend::new(),
                observes: AtomicUsize::new(0),
                acts: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
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
            self.observes.fetch_add(1, Ordering::SeqCst);
            panic!("an unattested backend must never receive observation dispatch")
        }

        async fn act(
            &self,
            _run_id: &str,
            _observation: &ComputerObservation,
            _action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.acts.fetch_add(1, Ordering::SeqCst);
            panic!("an unattested backend must never receive action dispatch")
        }

        async fn act_if_current(
            &self,
            _run_id: &str,
            _observation: &ComputerObservation,
            _action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.acts.fetch_add(1, Ordering::SeqCst);
            panic!("an unattested backend must never receive action dispatch")
        }

        async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            panic!("an unattested backend must never receive cancellation dispatch")
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for NativeForegroundFixture {
        fn capabilities(&self) -> ComputerCapabilities {
            ComputerCapabilities::from_proof(macos_native_capability_proof())
                .expect("native foreground fixture proof is valid")
        }

        fn physical_input_domain(&self) -> crate::computer_use::PhysicalInputDomain {
            macos_native_physical_input_domain()
        }

        async fn observe(
            &self,
            run_id: &str,
            observation_id: &str,
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            self.observes.fetch_add(1, Ordering::SeqCst);
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
            self.acts.fetch_add(1, Ordering::SeqCst);
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
            self.observes.fetch_add(1, Ordering::SeqCst);
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
            self.acts.fetch_add(1, Ordering::SeqCst);
            Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven backend must not act",
            ))
        }

        async fn act_if_current(
            &self,
            _run_id: &str,
            _observation: &ComputerObservation,
            _action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            self.acts.fetch_add(1, Ordering::SeqCst);
            Err(ComputerError::new(
                ComputerErrorCode::ForbiddenAction,
                "unproven backend must not act",
            ))
        }

        async fn cancel(&self, _run_id: &str) -> ComputerResult<()> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn unproven_backend_cannot_create_or_observe() {
        let dir = tempdir().unwrap();
        let service = ComputerUseService::new(
            Arc::new(UnprovenBackend::default()),
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

    #[tokio::test]
    async fn forged_or_unproven_backend_cannot_dispatch_against_an_existing_native_run() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let native_backend = Arc::new(NativeForegroundFixture::new());
        let native = trusted_fixture_service(native_backend.clone(), store.clone());
        let matching = trusted_fixture_service(native_backend.clone(), store.clone());
        let (run, observation) = authorized_ready(&native, "native-dispatch", Some(8)).await;
        assert_eq!(run.capability_proof.backend_id(), MACOS_NATIVE_BACKEND_ID);
        assert_eq!(
            run.capability_proof.tier(),
            ComputerCapabilityTier::ForegroundSemantic
        );
        let before = native.get_run(&run.run_id).unwrap().unwrap();
        let forged = Arc::new(ForgedIsolatedBackend::new("forged-piggyback"));
        let unproven = Arc::new(UnprovenBackend::default());
        let forged_service = ComputerUseService::new(forged.clone(), store.clone());
        let unproven_service = ComputerUseService::new(unproven.clone(), store);

        let action = ComputerAction::SetValue {
            element_id: format!("{}-name", observation.observation_id),
            text: "Ada".into(),
        };
        for (label, attacker) in [("forged", &forged_service), ("unproven", &unproven_service)] {
            assert_eq!(
                attacker
                    .observe(
                        &format!("{label}-observe"),
                        &caller(&before, attacker),
                        &before.run_id,
                        before.version,
                    )
                    .await
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenAction,
                "{label} observe must fail closed"
            );
            assert_eq!(
                attacker
                    .act(
                        &format!("{label}-act"),
                        &caller(&before, attacker),
                        &before.run_id,
                        before.version,
                        &observation.observation_id,
                        action.clone(),
                    )
                    .await
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenAction,
                "{label} act must fail closed"
            );
            assert_eq!(
                attacker
                    .cancel(
                        &format!("{label}-cancel"),
                        &caller(&before, attacker),
                        &before.run_id,
                    )
                    .await
                    .unwrap_err()
                    .code,
                ComputerErrorCode::ForbiddenAction,
                "{label} cancel must fail closed"
            );
        }

        let after_attack = native.get_run(&before.run_id).unwrap().unwrap();
        assert_eq!(after_attack.version, before.version);
        assert_eq!(after_attack.state, before.state);
        assert_eq!(after_attack.action_count, before.action_count);
        assert_eq!(after_attack.control_epoch, before.control_epoch);
        assert_eq!(after_attack.authority_epoch, before.authority_epoch);
        assert_eq!(after_attack.current_observation, before.current_observation);
        assert_eq!(after_attack.last_error, before.last_error);
        assert_eq!(after_attack.audit, before.audit);
        assert_eq!(forged.observes.load(Ordering::SeqCst), 0);
        assert_eq!(forged.acts.load(Ordering::SeqCst), 0);
        assert_eq!(forged.cancels.load(Ordering::SeqCst), 0);
        assert_eq!(unproven.observes.load(Ordering::SeqCst), 0);
        assert_eq!(unproven.acts.load(Ordering::SeqCst), 0);
        assert_eq!(unproven.cancels.load(Ordering::SeqCst), 0);
        assert_eq!(native_backend.observes.load(Ordering::SeqCst), 1);
        assert_eq!(native_backend.acts.load(Ordering::SeqCst), 0);
        assert_eq!(native_backend.cancels.load(Ordering::SeqCst), 0);

        matching
            .act(
                "matching-act",
                &caller(&before, &matching),
                &before.run_id,
                before.version,
                &observation.observation_id,
                action,
            )
            .await
            .unwrap();
        assert_eq!(native_backend.acts.load(Ordering::SeqCst), 1);
        let after_act = matching.get_run(&before.run_id).unwrap().unwrap();
        matching
            .cancel(
                "matching-cancel",
                &caller(&after_act, &matching),
                &after_act.run_id,
            )
            .await
            .unwrap();
        assert_eq!(native_backend.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(
            matching.get_run(&before.run_id).unwrap().unwrap().state,
            ComputerRunState::Cancelled
        );
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
            .pause(
                "pause-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        assert!(paused.control_epoch > run.control_epoch);
        assert_eq!(backend.cancels.load(Ordering::SeqCst), 1);
        let replayed = service
            .pause(
                "pause-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        assert_eq!(paused, replayed);
        assert_eq!(backend.cancels.load(Ordering::SeqCst), 1);

        let (run, _) = authorized_ready(&service, "takeover-table", None).await;
        let taken = service
            .take_over(
                "takeover-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let cancels = backend.cancels.load(Ordering::SeqCst);
        let replayed = service
            .take_over(
                "takeover-once",
                &caller(&run, &service),
                &run.run_id,
                run.version,
            )
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
