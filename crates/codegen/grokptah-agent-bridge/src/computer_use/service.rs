use std::sync::Arc;

use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::policy::ComputerPolicy;
use super::store::{ComputerStore, MutationClaim};
use super::types::{
    validate_id, ActionGrant, ActionOutcome, ComputerAction, ComputerBackend, ComputerError,
    ComputerErrorCode, ComputerObservation, ComputerResult, ComputerRun, ComputerRunState,
    ComputerTarget, ComputerUseLimits,
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
        target: ComputerTarget,
        limits: ComputerUseLimits,
    ) -> ComputerResult<ComputerRun> {
        target.validate()?;
        limits.validate()?;
        let payload = json!({
            "ownerSessionId": owner_session_id,
            "target": target,
            "limits": limits,
        });
        if let Some(replayed) = self.begin_mutation(request_id, "create_run", &payload)? {
            return replayed;
        }
        let result = (|| {
            self.store.can_create_run()?;
            let mut run = ComputerRun::new(owner_session_id, target, limits)?;
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
                self.policy.authorize_grant(run, &grant, Utc::now())?;
                run.grant = Some(grant.clone());
                run.last_error = None;
                run.transition(ComputerRunState::Ready)?;
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
                let observed = self
                    .backend
                    .observe(run_id, &prepared.target, &prepared.limits)
                    .await;
                match observed {
                    Ok(observation) => {
                        let validated = observation
                            .validate(&prepared.limits)
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
                let outcome = self.backend.act(run_id, &observation, &action).await;
                match outcome {
                    Ok(outcome) => self.commit_action(run_id, &action, &observation, outcome),
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
                run.transition(ComputerRunState::Paused)?;
                revoke_authority(run);
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
        outcome: ActionOutcome,
    ) -> ComputerResult<ActionOutcome> {
        self.store
            .update_run(run_id, |run| {
                if run.state != ComputerRunState::Acting {
                    return Err(ComputerError::new(
                        ComputerErrorCode::UncertainOutcome,
                        "action completed after the run was cancelled or superseded",
                    ));
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
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            let mut observation = self.inner.observe(run_id, target, limits).await?;
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
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> ComputerResult<ComputerObservation> {
            self.inner.observe(run_id, target, limits).await
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
    async fn simulator_run_is_durable_bounded_and_replay_safe() {
        let (backend, service) = service();
        let run = service
            .create_run(
                "create-1",
                Uuid::new_v4(),
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
        assert!(taken_over.grant.unwrap().revoked_at.is_some());
        assert!(taken_over.audit.iter().any(|entry| {
            entry.operation == "take_over" && entry.disposition == "operator_control"
        }));
    }

    #[tokio::test]
    async fn denied_action_is_audited_without_storing_entered_text() {
        let (_backend, service) = service();
        let run = service
            .create_run(
                "create-denied",
                Uuid::new_v4(),
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
        let error = action.await.unwrap().unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
        let persisted = service.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(persisted.state, ComputerRunState::Cancelled);
        assert_eq!(persisted.action_count, 0);
        assert!(persisted.current_observation.is_none());
    }
}
