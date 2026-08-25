//! Host/orchestration boundary for external workers.
//!
//! This layer owns durable idempotency, host repository allowlists, and
//! fail-closed Pending/Uncertain outcomes. It does not execute Computer Use
//! and it does not share the core agent harness turn loop.

use super::ledger::{
    canonical_cancel_payload_hash, canonical_follow_up_payload_hash, canonical_launch_payload_hash,
    ExternalWorkerLedger, ExternalWorkerLedgerClaim, ExternalWorkerOperation,
};
use super::{ExternalWorkerAdapter, ExternalWorkerAdapterError, ExternalWorkerRegistry};
use grokptah_agent_sdk::{
    ExternalWorkerArtifact, ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest,
    ExternalWorkerLaunchResult, ExternalWorkerRunRecord,
};
use reqwest::StatusCode;
use std::path::Path;
use std::sync::Arc;

/// Trusted host that wraps qualified adapters with a durable ledger.
pub struct ExternalWorkerHost {
    registry: Arc<ExternalWorkerRegistry>,
    ledger: ExternalWorkerLedger,
}

impl ExternalWorkerHost {
    /// Open a host against an explicit registry and durable ledger root.
    pub fn open(
        registry: Arc<ExternalWorkerRegistry>,
        root: impl AsRef<Path>,
    ) -> Result<Self, ExternalWorkerAdapterError> {
        Ok(Self {
            registry,
            ledger: ExternalWorkerLedger::open(root)?,
        })
    }

    /// Launch through the host allowlist and idempotency ledger.
    pub async fn launch(
        &self,
        request: &ExternalWorkerLaunchRequest,
    ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        if !self
            .registry
            .repository_allowed(request.provider, &request.repository)?
        {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "repository is not in the host allowlist",
            ));
        }
        let adapter = self
            .registry
            .get(request.provider)
            .ok_or(ExternalWorkerAdapterError::UnsupportedProvider)?;
        let hash = canonical_launch_payload_hash(request)?;
        match self
            .ledger
            .claim(ExternalWorkerOperation::Launch, &request.request_id, &hash)?
        {
            ExternalWorkerLedgerClaim::Replay(value) => replay_launch(value),
            ExternalWorkerLedgerClaim::ReplayError(error) => Err(error),
            ExternalWorkerLedgerClaim::Perform => match adapter.launch(request).await {
                Ok(result) => {
                    let value = serde_json::to_value(&result).map_err(|_| {
                        ExternalWorkerAdapterError::InvalidResponse(
                            "launch result could not be persisted",
                        )
                    })?;
                    self.ledger.complete(
                        ExternalWorkerOperation::Launch,
                        &request.request_id,
                        &hash,
                        value,
                    )?;
                    Ok(result)
                }
                Err(error) => Err(self.record_mutation_error(
                    ExternalWorkerOperation::Launch,
                    &request.request_id,
                    &hash,
                    error,
                )),
            },
        }
    }

    /// Queue a follow-up through the ledger.
    pub async fn follow_up(
        &self,
        external_agent_id: &str,
        request: &ExternalWorkerFollowUpRequest,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        let adapter = self.cursor_adapter()?;
        let hash = canonical_follow_up_payload_hash(external_agent_id, request)?;
        match self.ledger.claim(
            ExternalWorkerOperation::FollowUp,
            &request.request_id,
            &hash,
        )? {
            ExternalWorkerLedgerClaim::Replay(value) => replay_run(value),
            ExternalWorkerLedgerClaim::ReplayError(error) => Err(error),
            ExternalWorkerLedgerClaim::Perform => {
                match adapter.follow_up(external_agent_id, request).await {
                    Ok(result) => {
                        let value = serde_json::to_value(&result).map_err(|_| {
                            ExternalWorkerAdapterError::InvalidResponse(
                                "follow-up result could not be persisted",
                            )
                        })?;
                        self.ledger.complete(
                            ExternalWorkerOperation::FollowUp,
                            &request.request_id,
                            &hash,
                            value,
                        )?;
                        Ok(result)
                    }
                    Err(error) => Err(self.record_mutation_error(
                        ExternalWorkerOperation::FollowUp,
                        &request.request_id,
                        &hash,
                        error,
                    )),
                }
            }
        }
    }

    /// Cancel through the ledger. Success requires an observed Cancelled run.
    pub async fn cancel(
        &self,
        request_id: &str,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        if request_id.trim().is_empty() {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "request_id must not be empty",
            ));
        }
        let adapter = self.cursor_adapter()?;
        let hash = canonical_cancel_payload_hash(external_agent_id, external_run_id);
        match self
            .ledger
            .claim(ExternalWorkerOperation::Cancel, request_id, &hash)?
        {
            ExternalWorkerLedgerClaim::Replay(value) => replay_run(value),
            ExternalWorkerLedgerClaim::ReplayError(error) => Err(error),
            ExternalWorkerLedgerClaim::Perform => {
                match adapter.cancel(external_agent_id, external_run_id).await {
                    Ok(result) => {
                        if result.state != grokptah_agent_sdk::ExternalWorkerState::Cancelled {
                            self.ledger.uncertain(
                                ExternalWorkerOperation::Cancel,
                                request_id,
                                &hash,
                            )?;
                            return Err(ExternalWorkerAdapterError::Uncertain);
                        }
                        let value = serde_json::to_value(&result).map_err(|_| {
                            ExternalWorkerAdapterError::InvalidResponse(
                                "cancel result could not be persisted",
                            )
                        })?;
                        self.ledger.complete(
                            ExternalWorkerOperation::Cancel,
                            request_id,
                            &hash,
                            value,
                        )?;
                        Ok(result)
                    }
                    Err(error) => Err(self.record_mutation_error(
                        ExternalWorkerOperation::Cancel,
                        request_id,
                        &hash,
                        error,
                    )),
                }
            }
        }
    }

    /// Read a worker through the installed adapter.
    pub async fn get_worker(
        &self,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
    ) -> Result<grokptah_agent_sdk::ExternalWorkerRecord, ExternalWorkerAdapterError> {
        let adapter = self
            .registry
            .get(provider)
            .ok_or(ExternalWorkerAdapterError::UnsupportedProvider)?;
        adapter.get_worker(external_agent_id).await
    }

    /// Read a run through the installed adapter.
    pub async fn get_run(
        &self,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        let adapter = self
            .registry
            .get(provider)
            .ok_or(ExternalWorkerAdapterError::UnsupportedProvider)?;
        adapter.get_run(external_agent_id, external_run_id).await
    }

    /// List run-attributed artifacts. Raw download URLs never cross this boundary.
    ///
    /// The listing is re-validated here rather than trusted from the adapter.
    /// Path containment, the digest rule, the size ceiling, run attribution,
    /// and the item ceiling are properties of the boundary, so an adapter that
    /// forgets one cannot publish through it.
    pub async fn list_artifacts(
        &self,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
        let adapter = self
            .registry
            .get(provider)
            .ok_or(ExternalWorkerAdapterError::UnsupportedProvider)?;
        let artifacts = adapter
            .list_artifacts(external_agent_id, external_run_id)
            .await?;
        grokptah_agent_sdk::validate_artifact_listing(&artifacts, external_run_id)
            .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
        Ok(artifacts)
    }

    fn cursor_adapter(&self) -> Result<Arc<dyn ExternalWorkerAdapter>, ExternalWorkerAdapterError> {
        self.registry
            .get(grokptah_agent_sdk::ExternalWorkerProvider::CursorCloud)
            .ok_or(ExternalWorkerAdapterError::UnsupportedProvider)
    }

    fn record_mutation_error(
        &self,
        operation: ExternalWorkerOperation,
        request_id: &str,
        hash: &str,
        error: ExternalWorkerAdapterError,
    ) -> ExternalWorkerAdapterError {
        let persist = if mutation_outcome_uncertain(&error) {
            self.ledger.uncertain(operation, request_id, hash)
        } else {
            self.ledger.fail(operation, request_id, hash, &error)
        };
        if persist.is_err() {
            return ExternalWorkerAdapterError::Uncertain;
        }
        if mutation_outcome_uncertain(&error) {
            ExternalWorkerAdapterError::Uncertain
        } else {
            error
        }
    }
}

fn mutation_outcome_uncertain(error: &ExternalWorkerAdapterError) -> bool {
    match error {
        ExternalWorkerAdapterError::Uncertain
        | ExternalWorkerAdapterError::Transport(_)
        | ExternalWorkerAdapterError::InvalidResponse(_) => true,
        ExternalWorkerAdapterError::Provider { status, .. } => {
            *status == StatusCode::CONFLICT || status.is_server_error()
        }
        _ => false,
    }
}

fn replay_launch(
    value: serde_json::Value,
) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
    let result: ExternalWorkerLaunchResult = serde_json::from_value(value).map_err(|_| {
        ExternalWorkerAdapterError::InvalidResponse("persisted launch result is invalid")
    })?;
    result
        .validate()
        .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
    Ok(result)
}

fn replay_run(
    value: serde_json::Value,
) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
    let result: ExternalWorkerRunRecord = serde_json::from_value(value).map_err(|_| {
        ExternalWorkerAdapterError::InvalidResponse("persisted run result is invalid")
    })?;
    result
        .validate()
        .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_worker::cursor::{
        spawn_fake_cursor, CursorCloudAdapter, FakeCursorState, FAKE_AGENT, FAKE_RUN,
    };
    use grokptah_agent_sdk::{
        ExternalWorkerExecutionMode, ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest,
        ExternalWorkerProvider, ExternalWorkerState,
    };
    use std::sync::Arc;

    fn launch_request(request_id: &str, prompt: &str) -> ExternalWorkerLaunchRequest {
        ExternalWorkerLaunchRequest {
            request_id: request_id.into(),
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "main".into(),
            prompt: prompt.into(),
            model: None,
            execution_mode: ExternalWorkerExecutionMode::Isolated,
            auto_create_pr: false,
            bounds: None,
        }
    }

    async fn host_with_fake(
        state: FakeCursorState,
    ) -> (ExternalWorkerHost, FakeCursorState, tempfile::TempDir) {
        let base = spawn_fake_cursor(state.clone()).await;
        let adapter = Arc::new(CursorCloudAdapter::for_test(&base));
        let registry = Arc::new(ExternalWorkerRegistry::new());
        registry.register(adapter);
        registry
            .set_repository_allowlist(ExternalWorkerProvider::CursorCloud, ["chriscase/GrokPtah"])
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let host = ExternalWorkerHost::open(registry, dir.path()).unwrap();
        (host, state, dir)
    }

    #[tokio::test]
    async fn retries_do_not_duplicate_remote_launches() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        let request = launch_request("req-1", "do the work");
        let first = host.launch(&request).await.unwrap();
        let second = host.launch(&request).await.unwrap();
        assert_eq!(first.run.external_run_id, second.run.external_run_id);
        assert_eq!(state.launch_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn payload_drift_is_rejected_without_a_second_launch() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        host.launch(&launch_request("req-1", "do the work"))
            .await
            .unwrap();
        let error = host
            .launch(&launch_request("req-1", "different work"))
            .await
            .unwrap_err();
        assert!(matches!(error, ExternalWorkerAdapterError::PayloadDrift));
        assert_eq!(state.launch_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn host_allowlist_rejects_arbitrary_accessible_repositories() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        let mut request = launch_request("req-other", "do the work");
        request.repository = "other/repo".into();
        let error = host.launch(&request).await.unwrap_err();
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::InvalidRequest("repository is not in the host allowlist")
        ));
        assert!(state.launch_requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn concurrent_identical_launches_fail_closed_on_pending() {
        let state = FakeCursorState::default();
        state.config.lock().unwrap().create_delay_ms = 80;
        let (host, state, _dir) = host_with_fake(state).await;
        let request = launch_request("req-concurrent", "do the work");
        let host = Arc::new(host);
        let left = host.clone();
        let right = host.clone();
        let request_left = request.clone();
        let request_right = request;
        let (first, second) = tokio::join!(
            async move { left.launch(&request_left).await },
            async move { right.launch(&request_right).await }
        );
        let results = [first, second];
        let successes = results.iter().filter(|item| item.is_ok()).count();
        let pending = results
            .iter()
            .filter(|item| matches!(item, Err(ExternalWorkerAdapterError::Pending)))
            .count();
        assert_eq!(successes, 1);
        assert_eq!(pending, 1);
        assert_eq!(state.launch_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancel_replay_stays_terminal_cancelled() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        let launched = host
            .launch(&launch_request("req-launch", "do the work"))
            .await
            .unwrap();
        let first = host
            .cancel("req-cancel", FAKE_AGENT, &launched.run.external_run_id)
            .await
            .unwrap();
        let second = host
            .cancel("req-cancel", FAKE_AGENT, &launched.run.external_run_id)
            .await
            .unwrap();
        assert_eq!(first.state, ExternalWorkerState::Cancelled);
        assert_eq!(second.state, ExternalWorkerState::Cancelled);
        assert_eq!(*state.cancel_calls.lock().unwrap(), 1);
        assert_eq!(first.external_run_id, FAKE_RUN);
    }

    #[tokio::test]
    async fn follow_up_retries_do_not_duplicate_remote_runs() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        host.launch(&launch_request("req-launch", "do the work"))
            .await
            .unwrap();
        let request = ExternalWorkerFollowUpRequest {
            request_id: "req-follow".into(),
            prompt: "re-check the focused change".into(),
            bounds: None,
        };
        let first = host.follow_up(FAKE_AGENT, &request).await.unwrap();
        let second = host.follow_up(FAKE_AGENT, &request).await.unwrap();
        assert_eq!(first.external_run_id, second.external_run_id);
        assert_eq!(state.follow_up_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancel_payload_drift_is_rejected() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        host.launch(&launch_request("req-launch", "do the work"))
            .await
            .unwrap();
        host.cancel("req-cancel", FAKE_AGENT, FAKE_RUN)
            .await
            .unwrap();
        let error = host
            .cancel("req-cancel", FAKE_AGENT, "run-other")
            .await
            .unwrap_err();
        assert!(matches!(error, ExternalWorkerAdapterError::PayloadDrift));
        assert_eq!(*state.cancel_calls.lock().unwrap(), 1);
    }

    /// An adapter that forgets a rule must not be able to publish through the
    /// host. Containment, the digest rule, the size ceiling, attribution, and
    /// the item ceiling are boundary properties, so the host re-checks them
    /// instead of trusting whatever the adapter returned.
    struct RogueArtifactAdapter {
        artifacts: Vec<ExternalWorkerArtifact>,
    }

    #[async_trait::async_trait]
    impl ExternalWorkerAdapter for RogueArtifactAdapter {
        fn provider(&self) -> ExternalWorkerProvider {
            ExternalWorkerProvider::LocalWorker
        }

        async fn launch(
            &self,
            _request: &ExternalWorkerLaunchRequest,
        ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
            unreachable!("this adapter exists only to return artifacts")
        }

        async fn get_worker(
            &self,
            _external_agent_id: &str,
        ) -> Result<grokptah_agent_sdk::ExternalWorkerRecord, ExternalWorkerAdapterError> {
            unreachable!("this adapter exists only to return artifacts")
        }

        async fn get_run(
            &self,
            _external_agent_id: &str,
            _external_run_id: &str,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            unreachable!("this adapter exists only to return artifacts")
        }

        async fn follow_up(
            &self,
            _external_agent_id: &str,
            _request: &ExternalWorkerFollowUpRequest,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            unreachable!("this adapter exists only to return artifacts")
        }

        async fn list_artifacts(
            &self,
            _external_agent_id: &str,
            _external_run_id: &str,
        ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
            Ok(self.artifacts.clone())
        }

        async fn cancel(
            &self,
            _external_agent_id: &str,
            _external_run_id: &str,
        ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
            unreachable!("this adapter exists only to return artifacts")
        }
    }

    fn sound_artifact() -> ExternalWorkerArtifact {
        ExternalWorkerArtifact {
            path: "artifacts/report.md".into(),
            digest: "sha256:be426b4d0bc6e0536d2bb2e8917792b442ac93cfa0ea7ff26a95e00b62a5af37"
                .into(),
            external_run_id: FAKE_RUN.into(),
            size_bytes: Some(12),
        }
    }

    async fn host_with_rogue_adapter(
        artifacts: Vec<ExternalWorkerArtifact>,
    ) -> (ExternalWorkerHost, tempfile::TempDir) {
        let registry = Arc::new(ExternalWorkerRegistry::new());
        registry.register(Arc::new(RogueArtifactAdapter { artifacts }));
        let dir = tempfile::tempdir().unwrap();
        let host = ExternalWorkerHost::open(registry, dir.path()).unwrap();
        (host, dir)
    }

    #[tokio::test]
    async fn host_revalidates_every_artifact_listing_an_adapter_returns() {
        let (host, _dir) = host_with_rogue_adapter(vec![sound_artifact()]).await;
        let listed = host
            .list_artifacts(ExternalWorkerProvider::LocalWorker, FAKE_AGENT, FAKE_RUN)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);

        for (label, rogue) in [
            (
                "another run's artifact",
                ExternalWorkerArtifact {
                    external_run_id: "run-somebody-else".into(),
                    ..sound_artifact()
                },
            ),
            (
                "a traversal path",
                ExternalWorkerArtifact {
                    path: "artifacts/../../etc/passwd".into(),
                    ..sound_artifact()
                },
            ),
            (
                "a drive-absolute path",
                ExternalWorkerArtifact {
                    path: "C:/Users/secret/.ssh/id_ed25519".into(),
                    ..sound_artifact()
                },
            ),
            (
                "an unverifiable digest",
                ExternalWorkerArtifact {
                    digest: "sha256:abc".into(),
                    ..sound_artifact()
                },
            ),
            (
                "an unbounded size",
                ExternalWorkerArtifact {
                    size_bytes: Some(u64::MAX),
                    ..sound_artifact()
                },
            ),
        ] {
            let (host, _dir) = host_with_rogue_adapter(vec![rogue]).await;
            let error = host
                .list_artifacts(ExternalWorkerProvider::LocalWorker, FAKE_AGENT, FAKE_RUN)
                .await
                .unwrap_err();
            assert!(
                matches!(error, ExternalWorkerAdapterError::InvalidResponse(_)),
                "{label} must be refused at the host boundary, got {error:?}",
            );
        }

        let over_ceiling =
            vec![sound_artifact(); grokptah_agent_sdk::MAX_EXTERNAL_WORKER_ARTIFACTS + 1];
        let (host, _dir) = host_with_rogue_adapter(over_ceiling).await;
        let error = host
            .list_artifacts(ExternalWorkerProvider::LocalWorker, FAKE_AGENT, FAKE_RUN)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                ExternalWorkerAdapterError::InvalidResponse(
                    "artifact listing exceeds its item ceiling"
                )
            ),
            "an over-long listing must be refused at the host boundary, got {error:?}",
        );
    }

    #[tokio::test]
    async fn provider_5xx_is_uncertain_and_retry_does_not_create_again() {
        let state = FakeCursorState::default();
        state.config.lock().unwrap().create_status = 500;
        let (host, state, _dir) = host_with_fake(state).await;
        let request = launch_request("req-500", "do the work");
        assert!(matches!(
            host.launch(&request).await.unwrap_err(),
            ExternalWorkerAdapterError::Uncertain
        ));
        assert!(matches!(
            host.launch(&request).await.unwrap_err(),
            ExternalWorkerAdapterError::Uncertain
        ));
        assert_eq!(state.launch_requests.lock().unwrap().len(), 1);
    }
}
