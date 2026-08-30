use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::adaptive::{self, AdaptiveApproval, AdaptiveClaim, AdaptiveDecisionRecord};
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

pub struct ComputerUseService {
    backend: Arc<dyn ComputerBackend>,
    store: ComputerStore,
    policy: ComputerPolicy,
}

impl ComputerUseService {
    pub fn new(backend: Arc<dyn ComputerBackend>, store: ComputerStore) -> Self {
        Self {
            backend,
            store,
            policy: ComputerPolicy,
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
                    revoke_authority(run);
                    run.record_audit("observe", "limit_reached", None, None, Some(error.code));
                    budget_error = Some(error);
                    return Ok(());
                }
                self.policy.authorize_observation(run, now)?;
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

    /// Dispatch one action. Unchanged: no adaptive review runs on this path.
    pub async fn act(
        &self,
        request_id: &str,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action: ComputerAction,
    ) -> ComputerResult<ActionOutcome> {
        self.act_inner(
            request_id,
            run_id,
            expected_version,
            observation_id,
            action,
            None,
        )
        .await
    }

    /// Dispatch one action with a planner claim attached.
    ///
    /// Identical to [`Self::act`] in every authority respect -- same
    /// idempotency receipt, same version fence, same staleness check, same
    /// policy gate, same state machine, same dispatch site -- plus one
    /// advisory review that runs after the policy gate has already admitted
    /// the action and can only refuse it. See [`super::adaptive`] for why that
    /// placement means a cheap model cannot buy its way past a kernel gate.
    pub async fn act_with_plan(
        &self,
        request_id: &str,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action: ComputerAction,
        claim: AdaptiveClaim,
    ) -> ComputerResult<ActionOutcome> {
        self.act_inner(
            request_id,
            run_id,
            expected_version,
            observation_id,
            action,
            Some(claim),
        )
        .await
    }

    /// Bind a host-supplied operator decision to the live stored run.
    ///
    /// This is the trusted-host integration seam for minting an
    /// [`AdaptiveApproval`]. The host must already hold the yes/no; this
    /// method does not prompt, persist a second send machine, or accept an
    /// approval token from JSON. It loads the current run and binds the
    /// boolean to that run's control epoch and current observation.
    ///
    /// Crate-private so planner, model, and wire types cannot call it. No
    /// production caller in this crate currently collects the operator
    /// decision; a host that does must call this and attach the token to the
    /// claim. This is not wired to UI, MCP, or a provider.
    #[allow(dead_code)]
    pub(crate) fn mint_host_adaptive_approval(
        &self,
        run_id: &str,
        approved: bool,
    ) -> ComputerResult<AdaptiveApproval> {
        validate_id("run_id", run_id)?;
        let run = self.store.load_run(run_id)?.ok_or_else(unknown_run)?;
        let observation = run.current_observation.as_ref().ok_or_else(|| {
            ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "computer run has no current observation to bind an approval to",
            )
        })?;
        Ok(AdaptiveApproval::host_mint(&run, observation, approved))
    }

    async fn act_inner(
        &self,
        request_id: &str,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action: ComputerAction,
        claim: Option<AdaptiveClaim>,
    ) -> ComputerResult<ActionOutcome> {
        validate_id("run_id", run_id)?;
        validate_id("observation_id", observation_id)?;
        action.validate(&ComputerUseLimits::ceiling())?;
        let payload = act_mutation_payload(
            run_id,
            expected_version,
            observation_id,
            &action,
            claim.as_ref(),
        )?;
        if let Some(replayed) = self.begin_mutation(request_id, "act", &payload)? {
            return replayed;
        }

        let mut budget_error = None;
        let mut review_record: Option<AdaptiveDecisionRecord> = None;
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
                self.policy
                    .authorize_action(run, &observation, &action, now)?;

                // ---- adaptive seam ------------------------------------
                // Reached only when the policy gate above already said yes,
                // so this can narrow that answer and has no path that widens
                // it. It mutates nothing but the decision record, drives no
                // state transition, and never retries: a resolution that is
                // not "commit" is returned as a refusal.
                if let Some(claim) = claim.as_ref() {
                    let outcome = adaptive::review(run, &observation, &action, claim, now);
                    if let Some(error) = outcome.refusal() {
                        // Carried out of the closure so the refused decision
                        // still reaches the denial write path: returning `Err`
                        // discards everything this closure wrote. Only a
                        // refusal is carried, so a later denial for an
                        // unrelated reason cannot be stamped with a record
                        // that says the review admitted the action.
                        review_record = Some(outcome.record().clone());
                        return Err(error.clone());
                    }
                    run.adaptive = Some(outcome.record().clone());
                }
                // ---- end adaptive seam --------------------------------

                if !backend_supports_action(&self.backend.capabilities(), action.class()) {
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
                let outcome = self.backend.act(run_id, &observation, &action).await;
                match outcome {
                    Ok(outcome) => {
                        self.commit_action(run_id, &action, &observation, control_epoch, outcome)
                    }
                    Err(error) => {
                        let error = classify_act_failure(error);
                        self.fail_inflight(run_id, "act", &error)?;
                        Err(error)
                    }
                }
            }
            (Err(error), _) => {
                self.record_denial_with_review(
                    run_id,
                    "act",
                    Some(action.class()),
                    &error,
                    review_record.take(),
                );
                Err(error)
            }
        };
        self.finish_mutation(request_id, &result)?;
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

    pub async fn cancel(&self, request_id: &str, run_id: &str) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let payload = json!({ "runId": run_id });
        if let Some(replayed) = self.begin_mutation(request_id, "cancel", &payload)? {
            return replayed;
        }
        let cancelled = self
            .store
            .update_run(run_id, |run| {
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
        run_id: &str,
        expected_version: u64,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id)?;
        let payload = json!({ "runId": run_id, "expectedVersion": expected_version });
        if let Some(replayed) = self.begin_mutation(request_id, "complete", &payload)? {
            return replayed;
        }
        let result = self
            .store
            .update_run(run_id, |run| {
                ensure_version(run, expected_version)?;
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
                // An ambiguous outcome is recorded as ambiguous in the audit
                // trail too. Writing "failed" beside a control disposition of
                // UncertainOutcome would leave the durable record disagreeing
                // with itself about whether the effect may have happened.
                let disposition = if error.code == ComputerErrorCode::UncertainOutcome {
                    run.set_control_disposition(ComputerControlDisposition::UncertainOutcome);
                    "uncertain_outcome"
                } else {
                    "failed"
                };
                run.record_audit(
                    operation,
                    disposition,
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
        self.record_denial_with_review(run_id, operation, action_class, error, None);
    }

    /// The same single denial write path, additionally stamping the adaptive
    /// review that produced the refusal.
    ///
    /// Recording the refused decision here rather than through a new mutation
    /// keeps the receipt truthful without adding a second write path: a
    /// refusal discards the `act` closure's mutations by design, so the record
    /// would otherwise be lost and the projection would show only admissions.
    fn record_denial_with_review(
        &self,
        run_id: &str,
        operation: &str,
        action_class: Option<super::types::ActionClass>,
        error: &ComputerError,
        review: Option<AdaptiveDecisionRecord>,
    ) {
        let _ = self.store.update_run(run_id, |run| {
            run.updated_at = Utc::now();
            if let Some(review) = review.as_ref() {
                run.adaptive = Some(review.clone());
            }
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
        let encoded = match result {
            Ok(value) => serde_json::to_value(value).map_err(|error| {
                ComputerError::new(ComputerErrorCode::Internal, error.to_string())
            }),
            Err(error) => Err(error.clone()),
        };
        self.store.complete_mutation(request_id, &encoded)
    }
}

/// Replay identity for one `act` / `act_with_plan` mutation.
///
/// The plain `act` object is exactly `runId`, `expectedVersion`,
/// `observationId`, and `action`. Adaptive keys are added only when a plan
/// (and, separately, a host-minted approval) is attached, so receipts written
/// before this seam existed still hash byte-for-byte.
fn act_mutation_payload(
    run_id: &str,
    expected_version: u64,
    observation_id: &str,
    action: &ComputerAction,
    claim: Option<&AdaptiveClaim>,
) -> ComputerResult<serde_json::Value> {
    let mut payload = json!({
        "runId": run_id,
        "expectedVersion": expected_version,
        "observationId": observation_id,
        "action": action,
    });
    // The claim is part of the replay identity: reusing a request id with a
    // different plan must fail closed rather than replay the first answer.
    //
    // The key is added only when a plan is attached, so the plain `act`
    // payload -- and therefore every durable mutation receipt already
    // written against it -- hashes exactly as it did before this seam
    // existed. A `null` placeholder would have invalidated them all.
    if let Some(claim) = claim {
        let encoded = serde_json::to_value(claim).map_err(|_| {
            ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "adaptive claim is not serializable",
            )
        })?;
        payload["adaptiveClaim"] = encoded;
        // The opaque approval never crosses the wire. The marker carries
        // only the yes/no and a non-secret fingerprint of the hidden
        // run/epoch/observation binding, so two approved tokens bound to
        // different observations cannot collide.
        if let Some(marker) = claim.approval_marker() {
            payload["adaptiveApproval"] = marker;
        }
    }
    Ok(payload)
}

/// Classify a failure the backend reported *after* it was handed an action.
///
/// By this point the backend has already been asked to deliver a physical
/// input event, and the trait gives us no way to ask whether it got as far as
/// emitting one. An ordinary failure is therefore only truthful for codes that
/// cannot be raised after emission — an admission or permission refusal, a
/// target that was already gone, a backend that was never reached. Everything
/// else may have landed on the surface, and reporting it as an ordinary
/// failure would tell the caller the action definitely did not happen.
///
/// Promoting those to `UncertainOutcome` marks the run's control disposition
/// and leaves the mutation receipt non-replayable, so the action is never
/// retried automatically on the strength of a failure we cannot prove.
fn classify_act_failure(error: ComputerError) -> ComputerError {
    match error.code {
        // Raised while admitting the action, before any event can be emitted.
        ComputerErrorCode::InvalidRequest
        | ComputerErrorCode::InvalidState
        | ComputerErrorCode::Unauthorized
        | ComputerErrorCode::PermissionRequired
        | ComputerErrorCode::PermissionDenied
        | ComputerErrorCode::PermissionRevoked
        | ComputerErrorCode::UnsupportedPlatform
        | ComputerErrorCode::ForbiddenTarget
        | ComputerErrorCode::ForbiddenAction
        | ComputerErrorCode::SensitiveSurface
        | ComputerErrorCode::StaleObservation
        | ComputerErrorCode::LimitReached
        | ComputerErrorCode::Conflict
        | ComputerErrorCode::BackendUnavailable
        // Already ambiguous; keep the caller's own classification.
        | ComputerErrorCode::UncertainOutcome => error,
        // May have taken physical effect.
        //
        // `TargetChanged` and `TargetClosed` belong here, not above: on macOS
        // the accessibility API can report either *after* the input event was
        // dispatched — indeed a window closing is a plausible consequence of
        // the very click that was sent. Without a dispatch-phase marker from
        // the backend we cannot tell that from a pre-dispatch check, so the
        // safe reading is that the action may have landed.
        ComputerErrorCode::TargetChanged
        | ComputerErrorCode::TargetClosed
        | ComputerErrorCode::Pending
        | ComputerErrorCode::Interrupted
        | ComputerErrorCode::BackendFailure
        | ComputerErrorCode::Internal => ComputerError::new(
            ComputerErrorCode::UncertainOutcome,
            format!(
                "action outcome is uncertain after dispatch (backend reported {:?}); it will not be retried automatically",
                error.code
            ),
        ),
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
    }

    #[derive(Debug)]
    struct EvidenceBackend {
        inner: SimulatorBackend,
        bytes: parking_lot::Mutex<Vec<u8>>,
    }

    /// Observes normally, then fails in `act` with a chosen code, so the
    /// service's post-dispatch classification can be exercised directly.
    #[derive(Debug)]
    struct FailingActBackend {
        inner: SimulatorBackend,
        code: ComputerErrorCode,
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
            self.release_action.notified().await;
            self.inner.act(run_id, observation, action).await
        }

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
            self.release_action.notify_waiters();
            self.inner.cancel(run_id).await
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for FailingActBackend {
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
            _run_id: &str,
            _observation: &ComputerObservation,
            _action: &ComputerAction,
        ) -> ComputerResult<ActionOutcome> {
            Err(ComputerError::new(self.code, "backend act failed"))
        }

        async fn cancel(&self, run_id: &str) -> ComputerResult<()> {
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

    /// Counts backend `act` dispatches so a refused review can be distinguished
    /// from an action that happened anyway.
    #[derive(Debug, Default)]
    struct CountingBackend {
        inner: SimulatorBackend,
        action_calls: AtomicUsize,
    }

    impl CountingBackend {
        fn action_calls(&self) -> usize {
            self.action_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for CountingBackend {
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

    /// Every error code must have a deliberate post-dispatch classification.
    /// This match is exhaustive on purpose: adding a code forces a decision
    /// about whether it can be raised after an input event was emitted.
    #[test]
    fn act_failures_that_may_have_landed_are_not_ordinary_failures() {
        use ComputerErrorCode::*;

        // Raised while admitting the action; nothing can have been emitted.
        for code in [
            InvalidRequest,
            InvalidState,
            Unauthorized,
            PermissionRequired,
            PermissionDenied,
            PermissionRevoked,
            UnsupportedPlatform,
            ForbiddenTarget,
            ForbiddenAction,
            SensitiveSurface,
            StaleObservation,
            LimitReached,
            Conflict,
            BackendUnavailable,
        ] {
            let classified = classify_act_failure(ComputerError::new(code, "denied"));
            assert_eq!(
                classified.code, code,
                "{code:?} is a pre-emission refusal and must stay an ordinary failure"
            );
        }

        // May have taken physical effect before the error surfaced.
        for code in [
            TargetChanged,
            TargetClosed,
            Pending,
            Interrupted,
            BackendFailure,
            Internal,
        ] {
            let classified = classify_act_failure(ComputerError::new(code, "boom"));
            assert_eq!(
                classified.code, UncertainOutcome,
                "{code:?} can be raised after an event may have landed"
            );
            assert!(
                !classified.message.contains("boom"),
                "backend text must not be carried into the promoted error"
            );
        }

        // An outcome the backend already called ambiguous is left alone.
        let already = classify_act_failure(ComputerError::new(UncertainOutcome, "ambiguous"));
        assert_eq!(already.code, UncertainOutcome);
        assert_eq!(already.message, "ambiguous");
    }

    #[tokio::test]
    async fn a_backend_failure_during_act_leaves_the_run_uncertain_not_failed() {
        let dir = tempdir().unwrap().keep();
        let backend = Arc::new(FailingActBackend {
            inner: SimulatorBackend::new(),
            code: ComputerErrorCode::BackendFailure,
        });
        let service = ComputerUseService::new(
            backend,
            ComputerStore::open(dir.join("computer-use")).unwrap(),
        );
        let run = service
            .create_run(
                "create-uncertain",
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-uncertain", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-uncertain", &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();

        let error = service
            .act(
                "act-uncertain",
                &run.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .expect_err("the backend failed");

        // The input may already have landed on the surface, so the caller must
        // not be told the action definitely did not happen.
        assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(
            persisted.control_disposition,
            ComputerControlDisposition::UncertainOutcome
        );
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
    async fn a_permission_refusal_during_act_stays_an_ordinary_failure() {
        // The counterpart: a refusal the backend raises before emitting
        // anything must not be inflated into an ambiguous outcome.
        let dir = tempdir().unwrap().keep();
        let backend = Arc::new(FailingActBackend {
            inner: SimulatorBackend::new(),
            code: ComputerErrorCode::PermissionRevoked,
        });
        let service = ComputerUseService::new(
            backend,
            ComputerStore::open(dir.join("computer-use")).unwrap(),
        );
        let run = service
            .create_run(
                "create-refused",
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
        let run = service
            .authorize("grant-refused", &run.run_id, run.version, grant(&run))
            .unwrap();
        let observation = service
            .observe("observe-refused", &run.run_id, run.version)
            .await
            .unwrap();
        let current = service.get_run(&run.run_id).unwrap().unwrap();

        let error = service
            .act(
                "act-refused",
                &run.run_id,
                current.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .expect_err("the backend refused");
        assert_eq!(error.code, ComputerErrorCode::PermissionRevoked);
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(persisted.state, ComputerRunState::Failed);
        assert_ne!(
            persisted.control_disposition,
            ComputerControlDisposition::UncertainOutcome
        );
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
        backend.action_entered.notified().await;

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
        backend.action_entered.notified().await;

        let cancelled = service.cancel("cancel-race", &run.run_id).await.unwrap();
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

    fn counting_service() -> (Arc<CountingBackend>, ComputerUseService) {
        let dir = tempdir().unwrap().keep();
        let backend = Arc::new(CountingBackend::default());
        let service = ComputerUseService::new(
            backend.clone(),
            ComputerStore::open(dir.join("computer-use")).unwrap(),
        );
        (backend, service)
    }

    async fn authorized_observed_run(
        service: &ComputerUseService,
        request_prefix: &str,
        classes: BTreeSet<ActionClass>,
    ) -> (ComputerRun, ComputerObservation) {
        let run = service
            .create_run(
                &format!("{request_prefix}-create"),
                Uuid::new_v4(),
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let mut issued = grant(&run);
        issued.action_classes = classes;
        let run = service
            .authorize(
                &format!("{request_prefix}-grant"),
                &run.run_id,
                run.version,
                issued,
            )
            .unwrap();
        let observation = service
            .observe(
                &format!("{request_prefix}-observe"),
                &run.run_id,
                run.version,
            )
            .await
            .unwrap();
        let run = service.get_run(&run.run_id).unwrap().unwrap();
        (run, observation)
    }

    fn below_floor_claim(run: &ComputerRun, sequence: u64) -> AdaptiveClaim {
        AdaptiveClaim {
            profile: crate::computer_use::AdaptiveProfile::Balanced,
            planner: crate::computer_use::AdaptiveDisposition::Commit,
            assessment: crate::computer_use::AmbiguityAssessment::unambiguous(6_500),
            observed_control_epoch: run.control_epoch,
            observed_sequence: sequence,
            approval: None,
        }
    }

    fn name_action(observation: &ComputerObservation) -> ComputerAction {
        ComputerAction::SetValue {
            element_id: format!("{}-name", observation.observation_id),
            text: "Ada".into(),
        }
    }

    #[test]
    fn plain_act_mutation_payload_is_unchanged_when_no_plan_is_attached() {
        let action = ComputerAction::SetValue {
            element_id: "field".into(),
            text: "Ada".into(),
        };
        let payload = act_mutation_payload("run-1", 3, "obs-1", &action, None).unwrap();
        let expected = json!({
            "runId": "run-1",
            "expectedVersion": 3,
            "observationId": "obs-1",
            "action": action,
        });
        assert_eq!(payload, expected);
        assert_eq!(
            crate::orchestration::hash_payload(&payload),
            crate::orchestration::hash_payload(&expected)
        );
        assert!(payload.get("adaptiveClaim").is_none());
        assert!(payload.get("adaptiveApproval").is_none());
    }

    #[test]
    fn approval_bindings_are_distinct_in_the_act_replay_identity() {
        let now = Utc::now();
        let mut run = ComputerRun::new(
            Uuid::new_v4(),
            None,
            SimulatorBackend::demo_target(),
            Default::default(),
        )
        .unwrap();
        run.control_epoch = 4;
        let observation = ComputerObservation {
            observation_id: "obs-live".into(),
            sequence: 1,
            target: run.target.clone(),
            captured_at: now,
            geometry: crate::computer_use::ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements: Vec::new(),
            elements_truncated: false,
            sensitivity: crate::computer_use::Sensitivity::None,
        };
        let action = ComputerAction::SetValue {
            element_id: "field".into(),
            text: "Ada".into(),
        };
        let mut first = below_floor_claim(&run, 1);
        first.approval = Some(AdaptiveApproval::host_mint(&run, &observation, true));
        let mut other_observation = observation.clone();
        other_observation.observation_id = "obs-other".into();
        let mut second = below_floor_claim(&run, 1);
        second.approval = Some(AdaptiveApproval::host_mint(&run, &other_observation, true));

        let first_payload = act_mutation_payload(
            &run.run_id,
            1,
            &observation.observation_id,
            &action,
            Some(&first),
        )
        .unwrap();
        let second_payload = act_mutation_payload(
            &run.run_id,
            1,
            &observation.observation_id,
            &action,
            Some(&second),
        )
        .unwrap();
        assert_ne!(
            crate::orchestration::hash_payload(&first_payload),
            crate::orchestration::hash_payload(&second_payload)
        );
        let first_marker = first_payload["adaptiveApproval"].clone();
        let encoded = serde_json::to_string(&first_marker).unwrap();
        assert!(!encoded.contains(&run.run_id));
        assert!(!encoded.contains("obs-live"));
        assert!(!encoded.contains("obs-other"));
        assert_eq!(first_marker["approved"], json!(true));
        assert_ne!(
            first_marker["binding"],
            second_payload["adaptiveApproval"]["binding"]
        );
    }

    #[tokio::test]
    async fn host_mint_binds_only_to_the_live_run_epoch_and_observation() {
        let (backend, service) = counting_service();
        let error = service
            .mint_host_adaptive_approval("missing-run", true)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::InvalidRequest);

        let (run, observation) = authorized_observed_run(
            &service,
            "host-mint",
            BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
        )
        .await;
        let minted = service
            .mint_host_adaptive_approval(&run.run_id, true)
            .unwrap();
        let expected = AdaptiveApproval::host_mint(&run, &observation, true);
        assert_eq!(minted, expected);

        let next = service
            .observe("host-mint-observe-2", &run.run_id, run.version)
            .await
            .unwrap();
        assert_ne!(next.observation_id, observation.observation_id);
        let live = service.get_run(&run.run_id).unwrap().unwrap();
        let reminted = service
            .mint_host_adaptive_approval(&run.run_id, true)
            .unwrap();
        assert_eq!(
            reminted,
            AdaptiveApproval::host_mint(&live, &next, true)
        );
        assert_ne!(minted.binding_fingerprint(), reminted.binding_fingerprint());
        assert_eq!(backend.action_calls(), 0);
    }

    #[tokio::test]
    async fn mismatched_host_approval_never_reaches_the_backend() {
        let (backend, service) = counting_service();
        let (run, _observation) = authorized_observed_run(
            &service,
            "mismatch",
            BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
        )
        .await;
        let stale = service
            .mint_host_adaptive_approval(&run.run_id, true)
            .unwrap();
        let next = service
            .observe("mismatch-observe-2", &run.run_id, run.version)
            .await
            .unwrap();
        let live = service.get_run(&run.run_id).unwrap().unwrap();
        let mut claim = below_floor_claim(&live, next.sequence);
        claim.approval = Some(stale);

        let error = service
            .act_with_plan(
                "mismatch-act",
                &live.run_id,
                live.version,
                &next.observation_id,
                name_action(&next),
                claim,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::PermissionRequired);
        assert_eq!(backend.action_calls(), 0);

        let other = authorized_observed_run(
            &service,
            "mismatch-other",
            BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
        )
        .await
        .0;
        let foreign = service
            .mint_host_adaptive_approval(&other.run_id, true)
            .unwrap();
        let matching = service
            .mint_host_adaptive_approval(&live.run_id, true)
            .unwrap();
        let mut foreign_claim = below_floor_claim(&live, next.sequence);
        foreign_claim.approval = Some(foreign);
        let mut matching_claim = below_floor_claim(&live, next.sequence);
        matching_claim.approval = Some(matching);

        let first_payload = act_mutation_payload(
            &live.run_id,
            live.version,
            &next.observation_id,
            &name_action(&next),
            Some(&foreign_claim),
        )
        .unwrap();
        let second_payload = act_mutation_payload(
            &live.run_id,
            live.version,
            &next.observation_id,
            &name_action(&next),
            Some(&matching_claim),
        )
        .unwrap();
        assert_ne!(
            crate::orchestration::hash_payload(&first_payload),
            crate::orchestration::hash_payload(&second_payload)
        );

        let error = service
            .act_with_plan(
                "mismatch-replay",
                &live.run_id,
                live.version,
                &next.observation_id,
                name_action(&next),
                foreign_claim,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::PermissionRequired);
        let error = service
            .act_with_plan(
                "mismatch-replay",
                &live.run_id,
                live.version,
                &next.observation_id,
                name_action(&next),
                matching_claim,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Conflict);
        assert_eq!(backend.action_calls(), 0);
    }

    #[tokio::test]
    async fn matching_host_approval_still_admits_only_after_policy() {
        let (backend, service) = counting_service();
        let (run, observation) = authorized_observed_run(
            &service,
            "policy",
            BTreeSet::from([ActionClass::Semantic]),
        )
        .await;
        let approval = service
            .mint_host_adaptive_approval(&run.run_id, true)
            .unwrap();
        let mut claim = below_floor_claim(&run, observation.sequence);
        claim.approval = Some(approval);
        let error = service
            .act_with_plan(
                "policy-ungranted",
                &run.run_id,
                run.version,
                &observation.observation_id,
                name_action(&observation),
                claim,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);
        assert_eq!(backend.action_calls(), 0);

        let (run, observation) = authorized_observed_run(
            &service,
            "policy-ok",
            BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
        )
        .await;
        let approval = service
            .mint_host_adaptive_approval(&run.run_id, true)
            .unwrap();
        let mut claim = below_floor_claim(&run, observation.sequence);
        claim.approval = Some(approval);
        service
            .act_with_plan(
                "policy-granted",
                &run.run_id,
                run.version,
                &observation.observation_id,
                name_action(&observation),
                claim,
            )
            .await
            .unwrap();
        assert_eq!(backend.action_calls(), 1);
    }
}
