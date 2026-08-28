use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
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
use super::store::{ComputerStore, MutationClaim};
use super::types::{
    validate_id, ActionGrant, ActionOutcome, ComputerAction, ComputerBackend,
    ComputerControlDisposition, ComputerError, ComputerErrorCode, ComputerObservation,
    ComputerResult, ComputerRun, ComputerRunState, ComputerTarget, ComputerUseLimits,
};
use crate::capability_authority::{
    CapabilityAuthority, CapabilitySnapshot, ConsumedEffect, EffectLease, HostCapability,
};
use parking_lot::Mutex;

const SERVICE_ENVELOPE_ID: &str = "computer-service-envelope-v1";
const RUN_ENVELOPE_PREFIX: &str = "computer-run-envelope:";
const SETTLEMENT_ENVELOPE_PREFIX: &str = "computer-settlement-envelope:";
const SERVICE_PRINCIPAL_ID: &str = "host-principal";
const AUTH_GENERATION: u64 = 1;
const POLICY_GENERATION: &str = "computer-use-policy.v1";

const RUN_OPERATIONS: &[&str] = &[
    "authorize",
    "observe",
    "act",
    "pause",
    "take_over",
    "cancel",
    "complete",
    "settle_observation",
    "settle_action",
    "record_denial",
];
const SETTLEMENT_OPERATIONS: &[&str] = &["settle_observation", "settle_action", "record_denial"];

pub struct ComputerUseService {
    backend: Arc<dyn ComputerBackend>,
    store: ComputerStore,
    policy: ComputerPolicy,
    capability_authority: Arc<CapabilityAuthority>,
    service_capability: Option<RunCapability>,
    run_capabilities: Mutex<std::collections::HashMap<String, RunCapability>>,
    execution_receipts: Mutex<std::collections::HashMap<String, HostExecutionReceipt>>,
}

#[derive(Debug, Clone)]
struct RunCapability {
    envelope_id: String,
    settlement_envelope_id: String,
    snapshot: CapabilitySnapshot,
    capability: HostCapability,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct HostExecutionReceipt {
    run_id: String,
    observation_id: String,
    action_digest: String,
    effect: ConsumedEffect,
    outcome: ActionOutcome,
}

impl ComputerUseService {
    pub fn new(backend: Arc<dyn ComputerBackend>, store: ComputerStore) -> Self {
        Self::new_with_authority(backend, store, Arc::new(CapabilityAuthority::new(true)))
    }

    /// Construct a Computer Use service sharing the host's one authority.
    /// The authority remains opaque and process-owned; this constructor does
    /// not accept a caller-provided capability or generation.
    pub fn new_with_authority(
        backend: Arc<dyn ComputerBackend>,
        store: ComputerStore,
        capability_authority: Arc<CapabilityAuthority>,
    ) -> Self {
        let service_capability = CapabilitySnapshot::computer_use_service(
            SERVICE_PRINCIPAL_ID,
            &backend.capabilities(),
            POLICY_GENERATION,
        )
        .ok()
        .and_then(|snapshot| {
            let capability = capability_authority
                .issue(
                    &snapshot,
                    Utc::now(),
                    crate::capability_authority::DEFAULT_CAPABILITY_TTL,
                )
                .ok()?;
            capability_authority
                .install_envelope(
                    SERVICE_ENVELOPE_ID,
                    capability.clone(),
                    snapshot.clone(),
                    SERVICE_PRINCIPAL_ID,
                    AUTH_GENERATION,
                    POLICY_GENERATION,
                    RUN_OPERATIONS
                        .iter()
                        .copied()
                        .chain(["create_run", "settle_receipt"]),
                    "computer-host",
                    Utc::now(),
                )
                .ok()?;
            Some(RunCapability {
                envelope_id: SERVICE_ENVELOPE_ID.into(),
                settlement_envelope_id: SERVICE_ENVELOPE_ID.into(),
                snapshot,
                capability,
            })
        });
        Self {
            backend,
            store,
            policy: ComputerPolicy,
            capability_authority,
            service_capability,
            run_capabilities: Mutex::new(std::collections::HashMap::new()),
            execution_receipts: Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn capabilities(&self) -> super::types::ComputerCapabilities {
        self.backend.capabilities()
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

    pub async fn read_current_evidence(
        &self,
        run_id: &str,
        asset_id: &str,
    ) -> ComputerResult<Vec<u8>> {
        validate_id("run_id", run_id)?;
        validate_id("asset_id", asset_id)?;
        let run = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let capability = self.require_run_capability(&run)?;
        let lease = self.effect_lease(&capability, &run, "computer.capture")?;
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
        self.capability_authority
            .consume(lease, &capability.snapshot, Utc::now())
            .map_err(|error| {
                ComputerError::new(ComputerErrorCode::PermissionRevoked, error.to_string())
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
        owner_session_id: Uuid,
        workspace: Option<String>,
        target: ComputerTarget,
        limits: ComputerUseLimits,
    ) -> ComputerResult<ComputerRun> {
        target.validate()?;
        limits.validate()?;
        let payload = json!({
            "ownerSessionId": owner_session_id,
            "workspace": workspace.as_deref(),
            "target": target,
            "limits": limits,
        });
        if let Some(replayed) = self.begin_mutation(request_id, "create_run", &payload)? {
            return replayed;
        }
        let result = (|| {
            self.store.can_create_run()?;
            let mut run = ComputerRun::new(owner_session_id, workspace, target, limits)?;
            let capability = self.bind_run_capability(&run)?;
            self.consume_durable_lease("create_run", &payload)?;
            run.record_audit("create_run", "accepted", None, None, None);
            self.store.save_run(&run)?;
            self.run_capabilities
                .lock()
                .insert(run.run_id.clone(), capability);
            Ok(run)
        })();
        self.finish_mutation(request_id, &result)?;
        result
    }

    pub fn authorize(
        &self,
        request_id: &str,
        run_id: &str,
        expected_version: u64,
        grant: ActionGrant,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        grant.validate()?;
        let payload = json!({
            "runId": run_id,
            "expectedVersion": expected_version,
            "grant": grant,
        });
        if let Some(replayed) = self.begin_mutation(request_id, "authorize", &payload)? {
            return replayed;
        }
        let result = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                let _capability = self.ensure_run_capability(run)?;
                self.consume_durable_lease("authorize", &payload)?;
                if run.control_disposition == ComputerControlDisposition::OperatorTakeover {
                    return Err(ComputerError::new(
                        ComputerErrorCode::InvalidState,
                        "operator takeover is absorbing; create a new computer run",
                    ));
                }
                self.policy.authorize_grant(run, &grant, Utc::now())?;
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
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerObservation> {
        validate_id("run_id", run_id)?;
        let payload = json!({ "runId": run_id, "expectedVersion": expected_version });
        if let Some(replayed) = self.begin_mutation(request_id, "observe", &payload)? {
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
                    self.revoke_authority(run);
                    run.record_audit("observe", "limit_reached", None, None, Some(error.code));
                    budget_error = Some(error);
                    return Ok(());
                }
                self.policy.authorize_observation(run, now)?;
                let _capability = self.require_run_capability(run)?;
                self.consume_durable_lease("observe", &payload)?;
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
                let capability = self.require_run_capability(&prepared)?;
                let lease = self.effect_lease(&capability, &prepared, "computer.capture")?;
                let observed =
                    match self
                        .capability_authority
                        .consume(lease, &capability.snapshot, Utc::now())
                    {
                        // The lease is consumed immediately before the backend
                        // capture. No async wait may occur between this check and
                        // the effect boundary.
                        Ok(_) => {
                            self.backend
                                .observe(
                                    run_id,
                                    &observation_id,
                                    &prepared.target,
                                    &prepared.limits,
                                )
                                .await
                        }
                        Err(error) => Err(ComputerError::new(
                            ComputerErrorCode::PermissionRevoked,
                            error.to_string(),
                        )),
                    };
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
                        .map(|()| observation);
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
        if let Some(replayed) = self.begin_mutation(request_id, "act", &payload)? {
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
                    self.revoke_authority(run);
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
                self.policy
                    .authorize_action(run, &observation, &action, now)?;
                if !backend_supports_action(&self.backend.capabilities(), action.class()) {
                    return Err(ComputerError::new(
                        ComputerErrorCode::ForbiddenAction,
                        "the backend does not support this action class",
                    ));
                }
                let _capability = self.require_run_capability(run)?;
                self.consume_durable_lease("act", &payload)?;
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
                let capability = match self.require_run_capability(&prepared) {
                    Ok(capability) => capability,
                    Err(error) => {
                        self.fail_inflight(run_id, "act", &error)?;
                        self.finish_mutation::<ActionOutcome>(request_id, &Err(error.clone()))?;
                        return Err(error);
                    }
                };
                let lease = match self.effect_lease(&capability, &prepared, "computer.input") {
                    Ok(lease) => lease,
                    Err(error) => {
                        self.fail_inflight(run_id, "act", &error)?;
                        self.finish_mutation::<ActionOutcome>(request_id, &Err(error.clone()))?;
                        return Err(error);
                    }
                };
                let effect =
                    match self
                        .capability_authority
                        .consume(lease, &capability.snapshot, Utc::now())
                    {
                        Ok(effect) => effect,
                        Err(error) => {
                            self.fail_inflight(
                                run_id,
                                "act",
                                &ComputerError::new(
                                    ComputerErrorCode::PermissionRevoked,
                                    error.to_string(),
                                ),
                            )?;
                            return Err(ComputerError::new(
                                ComputerErrorCode::PermissionRevoked,
                                error.to_string(),
                            ));
                        }
                    };
                let outcome = self.backend.act(run_id, &observation, &action).await;
                match outcome {
                    Ok(outcome) => self.commit_action(
                        run_id,
                        &action,
                        &observation,
                        control_epoch,
                        effect,
                        outcome,
                    ),
                    Err(_error) => {
                        let uncertain = ComputerError::new(
                            ComputerErrorCode::UncertainOutcome,
                            "Computer input may have reached the backend; outcome is uncertain and will not be retried automatically",
                        );
                        self.fail_inflight(run_id, "act", &uncertain)?;
                        Err(uncertain)
                    }
                }
            }
            (Err(error), _) => {
                self.record_denial(run_id, "act", Some(action.class()), &error);
                Err(error)
            }
        };
        if let Err(finish_error) = self.finish_mutation(request_id, &result) {
            if result.is_ok() {
                return Err(finish_error);
            }
        }
        result
    }

    pub async fn pause(
        &self,
        request_id: &str,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let payload = json!({ "runId": run_id, "expectedVersion": expected_version });
        if let Some(replayed) = self.begin_mutation(request_id, "pause", &payload)? {
            return replayed;
        }
        let paused = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                self.consume_durable_lease("pause", &payload)?;
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
            Ok(run) => {
                let capability = self.require_run_capability(&run)?;
                let lease = self.effect_lease(&capability, &run, "computer.cancel")?;
                self.capability_authority
                    .consume(lease, &capability.snapshot, Utc::now())
                    .map_err(|error| {
                        ComputerError::new(ComputerErrorCode::PermissionRevoked, error.to_string())
                    })?;
                let result = self.backend.cancel(run_id).await;
                self.revoke_action_capability(run_id);
                result.map(|()| run)
            }
            Err(error) => {
                self.record_denial(run_id, "pause", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    /// Immediately yields control to the local operator. This is deliberately
    /// distinct from pause in the durable audit trail even though both revoke
    /// all outstanding authority and cancel backend work.
    pub async fn take_over(
        &self,
        request_id: &str,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let payload = json!({ "runId": run_id, "expectedVersion": expected_version });
        if let Some(replayed) = self.begin_mutation(request_id, "take_over", &payload)? {
            return replayed;
        }
        let taken_over = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                self.consume_durable_lease("take_over", &payload)?;
                run.transition(ComputerRunState::Paused)?;
                revoke_authority(run);
                run.set_control_disposition(ComputerControlDisposition::OperatorTakeover);
                run.record_audit("take_over", "operator_control", None, None, None);
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        let result = match taken_over {
            Ok(run) => {
                let capability = self.require_run_capability(&run)?;
                let lease = self.effect_lease(&capability, &run, "computer.cancel")?;
                self.capability_authority
                    .consume(lease, &capability.snapshot, Utc::now())
                    .map_err(|error| {
                        ComputerError::new(ComputerErrorCode::PermissionRevoked, error.to_string())
                    })?;
                let result = self.backend.cancel(run_id).await;
                self.revoke_action_capability(run_id);
                result.map(|()| run)
            }
            Err(error) => {
                self.record_denial(run_id, "take_over", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    pub async fn cancel(&self, request_id: &str, run_id: &str) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let payload = json!({ "runId": run_id });
        if let Some(replayed) = self.begin_mutation(request_id, "cancel", &payload)? {
            return replayed;
        }
        let cancelled = self
            .store
            .update_run(run_id, |run| {
                self.consume_durable_lease("cancel", &payload)?;
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
            Ok(run) => {
                if run.state.is_terminal() && !self.run_capabilities.lock().contains_key(run_id) {
                    Ok(run)
                } else {
                    let capability = self.require_run_capability(&run)?;
                    let lease = self.effect_lease(&capability, &run, "computer.cancel")?;
                    self.capability_authority
                        .consume(lease, &capability.snapshot, Utc::now())
                        .map_err(|error| {
                            ComputerError::new(
                                ComputerErrorCode::PermissionRevoked,
                                error.to_string(),
                            )
                        })?;
                    let result = self.backend.cancel(run_id).await;
                    self.revoke_action_capability(run_id);
                    result.map(|()| run)
                }
            }
            Err(error) => {
                self.record_denial(run_id, "cancel", None, &error);
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
        result
    }

    #[allow(dead_code)]
    pub(crate) fn complete_from_execution_receipt(
        &self,
        request_id: &str,
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let receipt = self
            .execution_receipts
            .lock()
            .get(run_id)
            .cloned()
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "completion requires a host-authenticated execution receipt",
                )
            })?;
        if receipt.run_id != run_id || receipt.outcome.expected_postcondition_met != Some(true) {
            return Err(ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "execution receipt does not authorize completion",
            ));
        }
        let payload = json!({
            "runId": run_id,
            "expectedVersion": expected_version,
            "observationId": receipt.observation_id,
            "actionDigest": receipt.action_digest,
            "effectLeaseId": receipt.effect.lease_id,
            "effectGeneration": receipt.effect.generation,
        });
        if let Some(replayed) = self.begin_mutation(request_id, "complete", &payload)? {
            return replayed;
        }
        let result = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
                self.consume_durable_lease("complete", &payload)?;
                run.transition(ComputerRunState::Completed)?;
                self.revoke_authority(run);
                run.record_audit("complete", "completed", None, None, None);
                Ok(())
            })
            .and_then(|run| run.ok_or_else(unknown_run));
        if let Err(error) = &result {
            self.record_denial(run_id, "complete", None, error);
        }
        self.finish_mutation(request_id, &result)?;
        if result.is_ok() {
            self.execution_receipts.lock().remove(run_id);
        }
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
        self.consume_durable_lease(
            "settle_observation",
            &json!({
                "runId": run_id,
                "observationId": observation.observation_id.clone(),
                "sequence": observation.sequence,
            }),
        )?;
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
                if run
                    .current_observation
                    .as_ref()
                    .is_some_and(|current| observation.sequence <= current.sequence)
                {
                    return Err(ComputerError::new(
                        ComputerErrorCode::StaleObservation,
                        "backend returned a nonmonotonic observation",
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
                    self.revoke_authority(run);
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
        effect: ConsumedEffect,
        outcome: ActionOutcome,
    ) -> ComputerResult<ActionOutcome> {
        let mut uncertain_error = None;
        let action_digest =
            crate::orchestration::hash_payload(&serde_json::to_value(action).map_err(|error| {
                ComputerError::new(ComputerErrorCode::Internal, error.to_string())
            })?);
        let receipt = HostExecutionReceipt {
            run_id: run_id.to_string(),
            observation_id: observation.observation_id.clone(),
            action_digest,
            effect,
            outcome: outcome.clone(),
        };
        self.consume_durable_lease(
            "settle_action",
            &json!({
                "runId": run_id,
                "observationId": observation.observation_id,
                "action": action,
                "controlEpoch": control_epoch,
            }),
        )?;
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
                    self.revoke_authority(run);
                } else if grant_exhausted {
                    run.transition(ComputerRunState::Paused)?;
                    self.revoke_authority(run);
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
        self.execution_receipts
            .lock()
            .insert(run_id.to_string(), receipt);
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
                self.revoke_authority(run);
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

    fn snapshot_for_run(&self, run: &ComputerRun) -> ComputerResult<CapabilitySnapshot> {
        let _ = run;
        self.service_capability
            .as_ref()
            .map(|binding| binding.snapshot.clone())
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "canonical computer capability envelope is unavailable",
                )
            })
    }

    fn bind_run_capability(&self, run: &ComputerRun) -> ComputerResult<RunCapability> {
        let service = self.service_capability.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::Unauthorized,
                "canonical computer capability envelope is unavailable",
            )
        })?;
        let envelope_id = format!("{RUN_ENVELOPE_PREFIX}{}", run.run_id);
        let settlement_envelope_id = format!("{SETTLEMENT_ENVELOPE_PREFIX}{}", run.run_id);
        self.capability_authority
            .install_envelope(
                &envelope_id,
                service.capability.clone(),
                service.snapshot.clone(),
                SERVICE_PRINCIPAL_ID,
                AUTH_GENERATION,
                POLICY_GENERATION,
                RUN_OPERATIONS.iter().copied(),
                &run.run_id,
                Utc::now(),
            )
            .map_err(|error| {
                ComputerError::new(ComputerErrorCode::Unauthorized, error.to_string())
            })?;
        self.capability_authority
            .install_envelope(
                &settlement_envelope_id,
                service.capability.clone(),
                service.snapshot.clone(),
                SERVICE_PRINCIPAL_ID,
                AUTH_GENERATION,
                POLICY_GENERATION,
                SETTLEMENT_OPERATIONS.iter().copied(),
                &run.run_id,
                Utc::now(),
            )
            .map_err(|error| {
                ComputerError::new(ComputerErrorCode::Unauthorized, error.to_string())
            })?;
        Ok(RunCapability {
            envelope_id,
            settlement_envelope_id,
            snapshot: service.snapshot.clone(),
            capability: service.capability.clone(),
        })
    }

    fn require_run_capability(&self, run: &ComputerRun) -> ComputerResult<RunCapability> {
        let capability = self
            .run_capabilities
            .lock()
            .get(&run.run_id)
            .cloned()
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "computer capability is unavailable; explicit reauthorization is required",
                )
            })?;
        let current = self.snapshot_for_run(run)?;
        self.capability_authority
            .revalidate_envelope(
                &capability.envelope_id,
                &capability.capability,
                &current,
                Utc::now(),
            )
            .map_err(|error| {
                ComputerError::new(
                    ComputerErrorCode::PermissionRevoked,
                    format!("computer capability is stale or revoked: {error}"),
                )
            })?;
        Ok(capability)
    }

    fn ensure_run_capability(&self, run: &ComputerRun) -> ComputerResult<RunCapability> {
        match self.require_run_capability(run) {
            Ok(capability) => Ok(capability),
            Err(error) if error.code == ComputerErrorCode::Unauthorized => {
                let binding = self.bind_run_capability(run)?;
                self.run_capabilities
                    .lock()
                    .insert(run.run_id.clone(), binding.clone());
                Ok(binding)
            }
            Err(error) => Err(error),
        }
    }

    fn revoke_run_capability(&self, run_id: &str) {
        if let Some(binding) = self.run_capabilities.lock().remove(run_id) {
            let _ = self
                .capability_authority
                .remove_envelope(&binding.envelope_id);
            let _ = self
                .capability_authority
                .remove_envelope(&binding.settlement_envelope_id);
        }
    }

    fn revoke_action_capability(&self, run_id: &str) {
        if let Some(binding) = self.run_capabilities.lock().get(run_id).cloned() {
            let _ = self
                .capability_authority
                .remove_envelope(&binding.envelope_id);
        }
    }

    fn revoke_authority(&self, run: &mut ComputerRun) {
        self.revoke_run_capability(&run.run_id);
        revoke_authority(run);
    }

    fn effect_lease(
        &self,
        capability: &RunCapability,
        run: &ComputerRun,
        effect_scope: &str,
    ) -> ComputerResult<EffectLease> {
        let current = self.snapshot_for_run(run)?;
        if current != capability.snapshot {
            return Err(ComputerError::new(
                ComputerErrorCode::PermissionRevoked,
                "computer capability snapshot changed before effect",
            ));
        }
        let operation = match effect_scope {
            "computer.capture" => "observe",
            "computer.input" => "act",
            "computer.cancel" => "cancel",
            _ => "act",
        };
        self.capability_authority
            .lease_from_envelope(
                &capability.envelope_id,
                operation,
                &run.run_id,
                effect_scope,
                Utc::now(),
                Duration::seconds(5),
            )
            .map_err(|error| {
                ComputerError::new(
                    ComputerErrorCode::PermissionRevoked,
                    format!("computer effect lease denied: {error}"),
                )
            })
    }

    fn consume_durable_claim(&self, operation: &str) -> ComputerResult<()> {
        let snapshot = self
            .service_capability
            .as_ref()
            .map(|binding| binding.snapshot.clone())
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "canonical durable capability envelope is unavailable",
                )
            })?;
        let lease = self
            .capability_authority
            .lease_from_envelope(
                SERVICE_ENVELOPE_ID,
                operation,
                "computer-host",
                &format!("durable.claim.{operation}"),
                Utc::now(),
                Duration::seconds(5),
            )
            .map_err(|error| {
                ComputerError::new(ComputerErrorCode::Unauthorized, error.to_string())
            })?;
        self.capability_authority
            .consume(lease, &snapshot, Utc::now())
            .map(|_| ())
            .map_err(|error| {
                ComputerError::new(ComputerErrorCode::PermissionRevoked, error.to_string())
            })
    }

    fn consume_durable_lease(
        &self,
        operation: &str,
        payload: &serde_json::Value,
    ) -> ComputerResult<()> {
        let run_id = payload
            .get("runId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let envelope_id = match run_id.as_deref() {
            Some(run_id)
                if matches!(
                    operation,
                    "settle_observation" | "settle_action" | "record_denial"
                ) =>
            {
                format!("{SETTLEMENT_ENVELOPE_PREFIX}{run_id}")
            }
            Some(run_id) => format!("{RUN_ENVELOPE_PREFIX}{run_id}"),
            None => SERVICE_ENVELOPE_ID.into(),
        };
        let resource = run_id.as_deref().unwrap_or("computer-host");
        let snapshot = self
            .service_capability
            .as_ref()
            .map(|binding| binding.snapshot.clone())
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "canonical durable capability envelope is unavailable",
                )
            })?;
        let lease = self
            .capability_authority
            .lease_from_envelope(
                &envelope_id,
                operation,
                resource,
                &format!("durable.{operation}"),
                Utc::now(),
                Duration::seconds(5),
            )
            .map_err(|error| {
                ComputerError::new(ComputerErrorCode::Unauthorized, error.to_string())
            })?;
        self.capability_authority
            .consume(lease, &snapshot, Utc::now())
            .map(|_| ())
            .map_err(|error| {
                ComputerError::new(ComputerErrorCode::PermissionRevoked, error.to_string())
            })
    }

    fn record_denial(
        &self,
        run_id: &str,
        operation: &str,
        action_class: Option<super::types::ActionClass>,
        error: &ComputerError,
    ) {
        if self
            .consume_durable_lease(
                "record_denial",
                &json!({
                    "runId": run_id,
                    "operation": operation,
                    "actionClass": action_class,
                    "errorCode": error.code,
                }),
            )
            .is_err()
        {
            return;
        }
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
    ) -> ComputerResult<Option<ComputerResult<T>>> {
        // Claiming the durable receipt is itself an effect. It has its own
        // bounded host lease rather than relying on admission alone.
        self.consume_durable_claim(operation)?;
        let hash = crate::orchestration::hash_payload(payload);
        match self.store.claim_mutation(request_id, operation, &hash)? {
            MutationClaim::Perform => Ok(None),
            MutationClaim::Pending => Ok(Some(Err(ComputerError::new(
                ComputerErrorCode::Pending,
                "an identical computer-use mutation is in progress",
            )))),
            MutationClaim::Uncertain => Ok(Some(Err(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "the earlier computer-use mutation has an uncertain outcome and will not be retried",
            )))),
            MutationClaim::Replay(result) => Ok(Some(match result {
                Ok(value) => serde_json::from_value(value).map_err(|error| {
                    ComputerError::new(ComputerErrorCode::Internal, error.to_string())
                }),
                Err(error) => Err(error),
            })),
        }
    }

    fn finish_mutation<T: Serialize>(
        &self,
        request_id: &str,
        result: &ComputerResult<T>,
    ) -> ComputerResult<()> {
        let result_kind = match result {
            Ok(_) => "success".to_string(),
            Err(error) => format!("{:?}", error.code),
        };
        self.consume_durable_lease(
            "settle_receipt",
            &json!({
                "requestId": request_id,
                "result": result_kind,
            }),
        )?;
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

fn backend_supports_action(
    capabilities: &super::types::ComputerCapabilities,
    action_class: super::types::ActionClass,
) -> bool {
    match action_class {
        super::types::ActionClass::Semantic => capabilities.semantic_actions,
        super::types::ActionClass::TextEntry => capabilities.text_entry,
        super::types::ActionClass::KeyChord => capabilities.key_chords,
        super::types::ActionClass::PointerFallback => capabilities.pointer_fallback,
    }
}

fn revoke_authority(run: &mut ComputerRun) {
    if let Some(grant) = &mut run.grant {
        grant.revoked_at.get_or_insert_with(Utc::now);
    }
    run.current_observation = None;
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
    use crate::computer_use::{ActionClass, ComputerCapabilities, EvidenceRef, SimulatorBackend};

    #[derive(Debug, Default)]
    struct BlockingBackend {
        inner: SimulatorBackend,
        action_entered: Notify,
        release_action: Notify,
        action_calls: AtomicUsize,
        action_release_requested: std::sync::atomic::AtomicBool,
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

    #[async_trait::async_trait]
    impl ComputerBackend for EvidenceBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            self.inner.capabilities()
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

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.inner.cancel(run_id).await
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for BlockingBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            self.inner.capabilities()
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
            loop {
                if self
                    .action_release_requested
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    break;
                }
                tokio::select! {
                    _ = self.release_action.notified() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
                }
            }
            let result = self.inner.act(run_id, observation, action).await;
            result
        }

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.action_release_requested.store(true, Ordering::SeqCst);
            self.release_action.notify_waiters();
            self.inner.cancel(run_id).await
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for MismatchedObservationBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            self.inner.capabilities()
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

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.inner.cancel(run_id).await
        }
    }

    fn service() -> (Arc<SimulatorBackend>, ComputerUseService) {
        let dir = tempdir().unwrap().keep();
        let backend = Arc::new(SimulatorBackend::new());
        let service = ComputerUseService::new(
            backend.clone(),
            ComputerStore::open(dir.join("computer-use")).unwrap(),
        );
        (backend, service)
    }

    fn grant(run: &ComputerRun) -> ActionGrant {
        let now = Utc::now();
        ActionGrant {
            grant_id: Uuid::new_v4().to_string(),
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            issued_by: crate::computer_use::GrantIssuer::LocalUser,
            issued_at: now,
            expires_at: now + Duration::minutes(5),
            uses_remaining: Some(8),
            revoked_at: None,
        }
    }

    #[test]
    fn arbitrary_durable_request_data_cannot_issue_authority() {
        let (_backend, service) = service();
        let before = service.capability_authority.generation_count_for_test();
        let error = service
            .consume_durable_lease("arbitrary_operation", &json!({"untrusted": "payload"}))
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Unauthorized);
        assert_eq!(
            service.capability_authority.generation_count_for_test(),
            before
        );
    }

    #[tokio::test]
    async fn revoking_the_run_generation_denies_before_backend_input() {
        let (backend, service) = service();
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-generation-revoke",
                owner,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-generation-revoke",
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe("observe-generation-revoke", &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let binding = service
            .run_capabilities
            .lock()
            .get(&run.run_id)
            .cloned()
            .unwrap();
        service
            .capability_authority
            .revoke(&binding.snapshot)
            .unwrap();

        let error = service
            .act(
                "act-generation-revoke",
                &run.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::PermissionRevoked);
        assert!(!backend.submitted());
        assert_eq!(
            service.get_run(&run.run_id).unwrap().unwrap().action_count,
            0
        );
    }

    #[tokio::test]
    async fn only_a_host_execution_receipt_can_settle_completion() {
        let (_backend, service) = service();
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-receipt-completion",
                owner,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-receipt-completion",
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let observation = service
            .observe("observe-receipt-completion", &run.run_id, run.version)
            .await
            .unwrap();
        service
            .act(
                "act-receipt-completion",
                &run.run_id,
                service.get_run(&run.run_id).unwrap().unwrap().version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        let run = service.get_run(&run.run_id).unwrap().unwrap();
        let completed = service
            .complete_from_execution_receipt(
                "complete-receipt-completion",
                &run.run_id,
                run.version,
            )
            .unwrap();
        assert_eq!(completed.state, ComputerRunState::Completed);
    }

    #[tokio::test]
    async fn a_restarted_service_cannot_reuse_an_old_run_capability() {
        let dir = tempdir().unwrap();
        let service = ComputerUseService::new(
            Arc::new(SimulatorBackend::new()),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-generation-restart",
                owner,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run_id = run.run_id.clone();
        drop(service);

        let restarted = ComputerUseService::new(
            Arc::new(SimulatorBackend::new()),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let recovered = restarted.get_run(&run_id).unwrap().unwrap();
        let error = restarted
            .observe("observe-generation-restart", &run_id, recovered.version)
            .await
            .unwrap_err();
        // Restart recovery interrupts the run before any stale in-memory
        // capability can be consulted; both fences deny the observation.
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
    }

    #[tokio::test]
    async fn backend_cannot_replace_the_host_minted_observation_identity() {
        let dir = tempdir().unwrap();
        let service = ComputerUseService::new(
            Arc::new(MismatchedObservationBackend::default()),
            ComputerStore::open(dir.path()).unwrap(),
        );
        let owner = Uuid::new_v4();
        let run = service
            .create_run(
                "create-host-id",
                owner,
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-host-id", &run.run_id, run.version, grant(&run))
            .unwrap();

        let error = service
            .observe("observe-host-id", &run.run_id, run.version)
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-1", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-1", &run.run_id, run.version)
            .await
            .unwrap();
        let after_observe = service.get_run(&run.run_id).unwrap().unwrap();
        let name_id = format!("{}-name", observation.observation_id);
        let outcome = service
            .act(
                "act-1",
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
            .observe("observe-2", &run.run_id, after_name.version)
            .await
            .unwrap();
        let after_observe = service.get_run(&run.run_id).unwrap().unwrap();
        service
            .act(
                "act-2",
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-conflict", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-conflict", &run.run_id, run.version)
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-pause", &run.run_id, run.version, grant(&run))
            .unwrap();
        let paused = service
            .pause("pause-1", &run.run_id, run.version)
            .await
            .unwrap();
        assert_eq!(paused.state, ComputerRunState::Paused);
        assert_eq!(
            paused.control_disposition,
            ComputerControlDisposition::Paused
        );
        assert!(paused.grant.unwrap().revoked_at.is_some());
        let error = service
            .observe("observe-paused", &run.run_id, paused.version)
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-takeover", &run.run_id, run.version, grant(&run))
            .unwrap();
        let taken_over = service
            .take_over("takeover-1", &run.run_id, run.version)
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-takeover-fence",
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let taken_over = service
            .take_over("takeover-fence", &run.run_id, run.version)
            .await
            .unwrap();

        let error = service
            .authorize(
                "stale-authorize-after-takeover",
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
            .pause("pause-after-takeover", &run.run_id, taken_over.version)
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let mut semantic_only = grant(&run);
        semantic_only.action_classes = BTreeSet::from([ActionClass::Semantic]);
        let run = service
            .authorize("grant-denied", &run.run_id, run.version, semantic_only)
            .unwrap();
        let observation = service
            .observe("observe-denied", &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let error = service
            .act(
                "deny-action",
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
        let service = ComputerUseService::new(
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                limits,
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-evidence-limit",
                &run.run_id,
                run.version,
                grant(&run),
            )
            .unwrap();
        let error = service
            .observe("observe-evidence-limit", &run.run_id, run.version)
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
        let service = ComputerUseService::new(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        );
        let run = service
            .create_run(
                "create-evidence-read",
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-evidence-read", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-evidence-read", &run.run_id, run.version)
            .await
            .unwrap();
        let evidence = observation.screenshot.unwrap();

        assert_eq!(
            service
                .read_current_evidence(&run.run_id, &evidence.asset_id)
                .await
                .unwrap(),
            b"ok"
        );
        assert_eq!(
            service
                .read_current_evidence(&run.run_id, "not-current")
                .await
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );

        *backend.bytes.lock() = b"no".to_vec();
        assert_eq!(
            service
                .read_current_evidence(&run.run_id, &evidence.asset_id)
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize(
                "grant-duration-limit",
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
            .observe("observe-duration-limit", &run.run_id, run.version)
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
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let mut one_use = grant(&run);
        one_use.uses_remaining = Some(1);
        let run = service
            .authorize("grant-one-use", &run.run_id, run.version, one_use)
            .unwrap();
        let observation = service
            .observe("observe-one-use", &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        service
            .act(
                "act-one-use",
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
        let service = Arc::new(ComputerUseService::new(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let run = service
            .create_run(
                "create-race",
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-race", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-race", &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let expected_version = current.version;
        let first_service = service.clone();
        let first_run_id = run.run_id.clone();
        let first_observation = observation.clone();
        let first = tokio::spawn(async move {
            first_service
                .act(
                    "act-race-first",
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
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            backend.action_entered.notified(),
        )
        .await
        .expect("action must reach the backend before cancellation");

        let error = service
            .act(
                "act-race-second",
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
        let service = Arc::new(ComputerUseService::new(
            backend.clone(),
            ComputerStore::open(dir.path().join("computer-use")).unwrap(),
        ));
        let run = service
            .create_run(
                "create-cancel-race",
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-cancel-race", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-cancel-race", &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();
        let action_service = service.clone();
        let action_run_id = run.run_id.clone();
        let action_observation = observation.clone();
        let action = tokio::spawn(async move {
            action_service
                .act(
                    "act-cancel-race",
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
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            backend.action_entered.notified(),
        )
        .await
        .expect("action must reach the backend before cancellation");
        tokio::task::yield_now().await;

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            service.cancel("cancel-race", &run.run_id),
        )
        .await
        .expect("cancel must release an in-flight action")
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
                owner,
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
                owner,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        service
            .create_run(
                "create-theirs",
                other,
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
        service.cancel("cancel-mine", &mine.run_id).await.unwrap();
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
                owner,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-parity", &run.run_id, run.version, grant(&run))
            .unwrap();
        service
            .observe("observe-parity", &run.run_id, run.version)
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
                owner,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-redaction", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-redaction", &run.run_id, run.version)
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
            let service = ComputerUseService::new(
                Arc::new(SimulatorBackend::new()),
                ComputerStore::open(dir.join("computer-use")).unwrap(),
            );
            let run = service
                .create_run(
                    "create-restart",
                    owner,
                    None,
                    SimulatorBackend::demo_target(),
                    Default::default(),
                )
                .unwrap();
            run_id = run.run_id.clone();
            service
                .authorize("grant-restart", &run.run_id, run.version, grant(&run))
                .unwrap();
        }

        let service = ComputerUseService::new(
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
}
