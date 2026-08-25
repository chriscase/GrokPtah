//! Host/orchestration boundary for external workers.
//!
//! This layer owns durable idempotency, host repository allowlists, and
//! fail-closed Pending/Uncertain outcomes. It does not execute Computer Use
//! and it does not share the core agent harness turn loop.

use super::authority::{
    AuthorityStore, ExternalWorkerAction, ExternalWorkerAuthority, ExternalWorkerPrincipal,
    LaunchIntent, NewGrant,
};
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

/// Trusted host that wraps qualified adapters with a durable ledger and the
/// durable authority every action is re-checked against.
///
/// Every method takes the authenticated [`ExternalWorkerPrincipal`] of the
/// caller. There is deliberately no method that accepts an opaque provider ID
/// alone: possession of an ID is not authority, so the type system does not
/// offer a way to act on one without also presenting a scope to check it
/// against.
pub struct ExternalWorkerHost {
    registry: Arc<ExternalWorkerRegistry>,
    ledger: ExternalWorkerLedger,
    authority: AuthorityStore,
}

impl ExternalWorkerHost {
    /// Open a host against an explicit registry and durable root.
    ///
    /// The ledger and the grant store are siblings under one root so a single
    /// process owns both; splitting them would let a replayed receipt outlive
    /// the grant that justified it.
    pub fn open(
        registry: Arc<ExternalWorkerRegistry>,
        root: impl AsRef<Path>,
    ) -> Result<Self, ExternalWorkerAdapterError> {
        let root = root.as_ref();
        Ok(Self {
            registry,
            ledger: ExternalWorkerLedger::open(root)?,
            authority: AuthorityStore::open(root.join("authority"))?,
        })
    }

    /// Re-authorize one action, returning the grant it was checked against.
    fn reauthorize(
        &self,
        action: ExternalWorkerAction,
        principal: &ExternalWorkerPrincipal,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
        external_run_id: Option<&str>,
    ) -> Result<ExternalWorkerAuthority, ExternalWorkerAdapterError> {
        Ok(self.authority.authorize(
            action,
            principal,
            provider,
            external_agent_id,
            external_run_id,
        )?)
    }

    /// Launch through the host allowlist and idempotency ledger, then issue
    /// the durable grant every later action on this worker is checked against.
    ///
    /// Launch authority comes from policy — a validated principal plus the host
    /// repository allowlist — because no grant exists yet. That allowlist is
    /// the *only* place it applies: it says a repository may be launched into,
    /// never that a caller may act on a worker that already exists.
    pub async fn launch(
        &self,
        principal: &ExternalWorkerPrincipal,
        run_id: &str,
        attempt: u32,
        request: &ExternalWorkerLaunchRequest,
    ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
        principal.validate()?;
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        if run_id.trim().is_empty() {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "external worker launch requires a GrokPtah run identity",
            ));
        }
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
            ExternalWorkerLedgerClaim::Perform => {
                // Everything up to here is provably un-sent, so an interrupted
                // attempt may auto-retry. Past this line the provider may have
                // acted, and recovery must reconcile instead.
                self.ledger.mark_sending(
                    ExternalWorkerOperation::Launch,
                    &request.request_id,
                    &hash,
                )?;
                match adapter.launch(request).await {
                    Ok(result) => {
                        let value = serde_json::to_value(&result).map_err(|_| {
                            ExternalWorkerAdapterError::InvalidResponse(
                                "launch result could not be persisted",
                            )
                        })?;
                        // The grant is written before the receipt completes. A
                        // worker that exists at the provider with no grant is
                        // ungovernable, so failing to record it is Uncertain — an
                        // operator reconciles — rather than a success that returns
                        // an ID nothing can later authorize.
                        let grant = ExternalWorkerAuthority::issue(NewGrant {
                            principal: principal.clone(),
                            provider: request.provider,
                            external_agent_id: result.worker.external_agent_id.clone(),
                            external_run_id: result.run.external_run_id.clone(),
                            run_id: run_id.to_string(),
                            attempt,
                            request_id: request.request_id.clone(),
                            launch_intent: LaunchIntent::from_request(request),
                            now: result.worker.created_at.clone(),
                        })?;
                        if self.authority.insert(&grant).is_err() {
                            self.ledger.uncertain(
                                ExternalWorkerOperation::Launch,
                                &request.request_id,
                                &hash,
                            )?;
                            return Err(ExternalWorkerAdapterError::Uncertain);
                        }
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
                }
            }
        }
    }

    /// Queue a follow-up through the ledger, under a re-checked grant.
    pub async fn follow_up(
        &self,
        principal: &ExternalWorkerPrincipal,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
        request: &ExternalWorkerFollowUpRequest,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        let mut grant = self.reauthorize(
            ExternalWorkerAction::FollowUp,
            principal,
            provider,
            external_agent_id,
            None,
        )?;
        let adapter = self.adapter(provider)?;
        let hash = canonical_follow_up_payload_hash(external_agent_id, request)?;
        match self.ledger.claim(
            ExternalWorkerOperation::FollowUp,
            &request.request_id,
            &hash,
        )? {
            ExternalWorkerLedgerClaim::Replay(value) => replay_run(value),
            ExternalWorkerLedgerClaim::ReplayError(error) => Err(error),
            ExternalWorkerLedgerClaim::Perform => {
                self.ledger.mark_sending(
                    ExternalWorkerOperation::FollowUp,
                    &request.request_id,
                    &hash,
                )?;
                match adapter.follow_up(external_agent_id, request).await {
                    Ok(result) => {
                        let value = serde_json::to_value(&result).map_err(|_| {
                            ExternalWorkerAdapterError::InvalidResponse(
                                "follow-up result could not be persisted",
                            )
                        })?;
                        // The new provider run joins the grant, so a later
                        // cancel or artifact read on it authorizes. A run the
                        // grant never admitted stays unreachable.
                        grant.admit_run(&result.external_run_id, result.updated_at.clone())?;
                        if self.authority.update(&grant).is_err() {
                            self.ledger.uncertain(
                                ExternalWorkerOperation::FollowUp,
                                &request.request_id,
                                &hash,
                            )?;
                            return Err(ExternalWorkerAdapterError::Uncertain);
                        }
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
        principal: &ExternalWorkerPrincipal,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        request_id: &str,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        if request_id.trim().is_empty() {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "request_id must not be empty",
            ));
        }
        self.reauthorize(
            ExternalWorkerAction::Cancel,
            principal,
            provider,
            external_agent_id,
            Some(external_run_id),
        )?;
        let adapter = self.adapter(provider)?;
        let hash = canonical_cancel_payload_hash(external_agent_id, external_run_id);
        match self
            .ledger
            .claim(ExternalWorkerOperation::Cancel, request_id, &hash)?
        {
            ExternalWorkerLedgerClaim::Replay(value) => replay_run(value),
            ExternalWorkerLedgerClaim::ReplayError(error) => Err(error),
            ExternalWorkerLedgerClaim::Perform => {
                self.ledger
                    .mark_sending(ExternalWorkerOperation::Cancel, request_id, &hash)?;
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

    /// Read a worker through the installed adapter, under a re-checked grant.
    ///
    /// Reads reauthorize too. A worker projection carries the repository, the
    /// branch, and a provider URL, so serving one to a caller outside the grant
    /// is the same disclosure as letting them steer it.
    pub async fn get_worker(
        &self,
        principal: &ExternalWorkerPrincipal,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
    ) -> Result<grokptah_agent_sdk::ExternalWorkerRecord, ExternalWorkerAdapterError> {
        self.reauthorize(
            ExternalWorkerAction::GetWorker,
            principal,
            provider,
            external_agent_id,
            None,
        )?;
        self.adapter(provider)?.get_worker(external_agent_id).await
    }

    /// Read a run through the installed adapter, under a re-checked grant.
    pub async fn get_run(
        &self,
        principal: &ExternalWorkerPrincipal,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        self.reauthorize(
            ExternalWorkerAction::GetRun,
            principal,
            provider,
            external_agent_id,
            Some(external_run_id),
        )?;
        self.adapter(provider)?
            .get_run(external_agent_id, external_run_id)
            .await
    }

    /// List run-attributed artifacts. Raw download URLs never cross this boundary.
    ///
    /// The listing is re-validated here rather than trusted from the adapter.
    /// Path containment, the digest rule, the size ceiling, run attribution,
    /// and the item ceiling are properties of the boundary, so an adapter that
    /// forgets one cannot publish through it.
    pub async fn list_artifacts(
        &self,
        principal: &ExternalWorkerPrincipal,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
        self.reauthorize(
            ExternalWorkerAction::ListArtifacts,
            principal,
            provider,
            external_agent_id,
            Some(external_run_id),
        )?;
        let adapter = self.adapter(provider)?;
        let artifacts = adapter
            .list_artifacts(external_agent_id, external_run_id)
            .await?;
        grokptah_agent_sdk::validate_artifact_listing(&artifacts, external_run_id)
            .map_err(ExternalWorkerAdapterError::InvalidResponse)?;
        Ok(artifacts)
    }

    /// Resolve the adapter for the provider the grant is bound to.
    ///
    /// This replaced a helper that always returned the Cursor adapter, which
    /// meant a follow-up or cancel on a worker created by any other provider
    /// was dispatched to Cursor with that other provider's opaque ID.
    fn adapter(
        &self,
        provider: grokptah_agent_sdk::ExternalWorkerProvider,
    ) -> Result<Arc<dyn ExternalWorkerAdapter>, ExternalWorkerAdapterError> {
        self.registry
            .get(provider)
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

    fn principal() -> ExternalWorkerPrincipal {
        ExternalWorkerPrincipal {
            principal: "user-1".into(),
            tenant: "tenant-a".into(),
            project: "project-x".into(),
            workspace: "/work/repo".into(),
            session_id: "session-1".into(),
            policy_revision: "policy-7".into(),
            capability_revision: "cap-3".into(),
            provider_account: "account-1".into(),
        }
    }

    const CURSOR: ExternalWorkerProvider = ExternalWorkerProvider::CursorCloud;

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
        let first = host
            .launch(&principal(), "gp-run-1", 1, &request)
            .await
            .unwrap();
        let second = host
            .launch(&principal(), "gp-run-1", 1, &request)
            .await
            .unwrap();
        assert_eq!(first.run.external_run_id, second.run.external_run_id);
        assert_eq!(state.launch_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn payload_drift_is_rejected_without_a_second_launch() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        host.launch(
            &principal(),
            "gp-run-1",
            1,
            &launch_request("req-1", "do the work"),
        )
        .await
        .unwrap();
        let error = host
            .launch(
                &principal(),
                "gp-run-1",
                1,
                &launch_request("req-1", "different work"),
            )
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
        let error = host
            .launch(&principal(), "gp-run-1", 1, &request)
            .await
            .unwrap_err();
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
            async move {
                left.launch(&principal(), "gp-run-1", 1, &request_left)
                    .await
            },
            async move {
                right
                    .launch(&principal(), "gp-run-1", 1, &request_right)
                    .await
            }
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
            .launch(
                &principal(),
                "gp-run-1",
                1,
                &launch_request("req-launch", "do the work"),
            )
            .await
            .unwrap();
        let first = host
            .cancel(
                &principal(),
                CURSOR,
                "req-cancel",
                FAKE_AGENT,
                &launched.run.external_run_id,
            )
            .await
            .unwrap();
        let second = host
            .cancel(
                &principal(),
                CURSOR,
                "req-cancel",
                FAKE_AGENT,
                &launched.run.external_run_id,
            )
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
        host.launch(
            &principal(),
            "gp-run-1",
            1,
            &launch_request("req-launch", "do the work"),
        )
        .await
        .unwrap();
        let request = ExternalWorkerFollowUpRequest {
            request_id: "req-follow".into(),
            prompt: "re-check the focused change".into(),
            bounds: None,
        };
        let first = host
            .follow_up(&principal(), CURSOR, FAKE_AGENT, &request)
            .await
            .unwrap();
        let second = host
            .follow_up(&principal(), CURSOR, FAKE_AGENT, &request)
            .await
            .unwrap();
        assert_eq!(first.external_run_id, second.external_run_id);
        assert_eq!(state.follow_up_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancelling_a_run_outside_the_grant_is_refused_before_the_ledger() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        host.launch(
            &principal(),
            "gp-run-1",
            1,
            &launch_request("req-launch", "do the work"),
        )
        .await
        .unwrap();
        host.cancel(&principal(), CURSOR, "req-cancel", FAKE_AGENT, FAKE_RUN)
            .await
            .unwrap();
        let error = host
            .cancel(&principal(), CURSOR, "req-cancel", FAKE_AGENT, "run-other")
            .await
            .unwrap_err();
        assert!(
            matches!(error, ExternalWorkerAdapterError::Unauthorized(_)),
            "an unadmitted run must be refused by authority, got {error:?}",
        );
        assert_eq!(*state.cancel_calls.lock().unwrap(), 1);
    }

    /// Payload drift still applies inside the grant: two runs the caller does
    /// own cannot share one cancel request_id.
    #[tokio::test]
    async fn cancel_payload_drift_is_rejected_within_the_grant() {
        let (host, state, _dir) = host_with_fake(FakeCursorState::default()).await;
        host.launch(
            &principal(),
            "gp-run-1",
            1,
            &launch_request("req-launch", "do the work"),
        )
        .await
        .unwrap();
        let follow_up = host
            .follow_up(
                &principal(),
                CURSOR,
                FAKE_AGENT,
                &ExternalWorkerFollowUpRequest {
                    request_id: "req-follow".into(),
                    prompt: "re-check the focused change".into(),
                    bounds: None,
                },
            )
            .await
            .unwrap();
        host.cancel(&principal(), CURSOR, "req-cancel", FAKE_AGENT, FAKE_RUN)
            .await
            .unwrap();
        let error = host
            .cancel(
                &principal(),
                CURSOR,
                "req-cancel",
                FAKE_AGENT,
                &follow_up.external_run_id,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, ExternalWorkerAdapterError::PayloadDrift),
            "a reused cancel request_id must drift, got {error:?}",
        );
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
        // This adapter never launches, so issue the grant the listing will be
        // re-authorized against directly.
        let grant = ExternalWorkerAuthority::issue(NewGrant {
            principal: principal(),
            provider: ExternalWorkerProvider::LocalWorker,
            external_agent_id: FAKE_AGENT.into(),
            external_run_id: FAKE_RUN.into(),
            run_id: "gp-run-1".into(),
            attempt: 1,
            request_id: "req-rogue".into(),
            launch_intent: LaunchIntent::from_request(&launch_request("req-rogue", "do the work")),
            now: "2026-08-25T00:00:00Z".into(),
        })
        .unwrap();
        host.authority.insert(&grant).unwrap();
        (host, dir)
    }

    /// Without a grant there is nothing to re-authorize against, so a caller
    /// holding a perfectly well-formed opaque ID still gets nothing.
    #[tokio::test]
    async fn an_ungranted_worker_is_refused_even_with_a_valid_looking_id() {
        let registry = Arc::new(ExternalWorkerRegistry::new());
        registry.register(Arc::new(RogueArtifactAdapter {
            artifacts: vec![sound_artifact()],
        }));
        let dir = tempfile::tempdir().unwrap();
        let host = ExternalWorkerHost::open(registry, dir.path()).unwrap();
        let error = host
            .list_artifacts(
                &principal(),
                ExternalWorkerProvider::LocalWorker,
                FAKE_AGENT,
                FAKE_RUN,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ExternalWorkerAdapterError::Unauthorized(_)));
    }

    /// The cross-tenant attack: a caller in another scope presents the exact
    /// opaque IDs that were minted for someone else.
    #[tokio::test]
    async fn a_foreign_scope_cannot_act_on_another_scopes_worker() {
        let (host, _dir) = host_with_rogue_adapter(vec![sound_artifact()]).await;
        host.list_artifacts(
            &principal(),
            ExternalWorkerProvider::LocalWorker,
            FAKE_AGENT,
            FAKE_RUN,
        )
        .await
        .expect("the issuing scope reads its own artifacts");

        type Mutate = fn(&mut ExternalWorkerPrincipal);
        let foreign_scopes: [Mutate; 7] = [
            |p| p.tenant = "tenant-b".into(),
            |p| p.project = "project-y".into(),
            |p| p.workspace = "/work/other".into(),
            |p| p.principal = "user-2".into(),
            |p| p.provider_account = "account-2".into(),
            |p| p.policy_revision = "policy-8".into(),
            |p| p.capability_revision = "cap-4".into(),
        ];
        for mutate in foreign_scopes {
            let mut foreign = principal();
            mutate(&mut foreign);
            let error = host
                .list_artifacts(
                    &foreign,
                    ExternalWorkerProvider::LocalWorker,
                    FAKE_AGENT,
                    FAKE_RUN,
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error, ExternalWorkerAdapterError::Unauthorized(_)),
                "a foreign scope must not inherit this worker, got {error:?}",
            );
        }
    }

    #[tokio::test]
    async fn host_revalidates_every_artifact_listing_an_adapter_returns() {
        let (host, _dir) = host_with_rogue_adapter(vec![sound_artifact()]).await;
        let listed = host
            .list_artifacts(
                &principal(),
                ExternalWorkerProvider::LocalWorker,
                FAKE_AGENT,
                FAKE_RUN,
            )
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
                .list_artifacts(
                    &principal(),
                    ExternalWorkerProvider::LocalWorker,
                    FAKE_AGENT,
                    FAKE_RUN,
                )
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
            .list_artifacts(
                &principal(),
                ExternalWorkerProvider::LocalWorker,
                FAKE_AGENT,
                FAKE_RUN,
            )
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
            host.launch(&principal(), "gp-run-1", 1, &request)
                .await
                .unwrap_err(),
            ExternalWorkerAdapterError::Uncertain
        ));
        assert!(matches!(
            host.launch(&principal(), "gp-run-1", 1, &request)
                .await
                .unwrap_err(),
            ExternalWorkerAdapterError::Uncertain
        ));
        assert_eq!(state.launch_requests.lock().unwrap().len(), 1);
    }
}
