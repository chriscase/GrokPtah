//! Manager commands for GrokPtah-managed external coding workers.
//!
//! These methods reuse `grokptah-agent-sdk::external_worker` DTOs and the
//! trusted `ExternalWorkerAdapter` registry. They persist opaque provider
//! identities on existing orchestration records, never grant Computer Use
//! authority, and never persist credentials or raw tool output.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use grokptah_agent_sdk::{
    ExternalWorkerEvent, ExternalWorkerExecutionMode, ExternalWorkerFollowUpRequest,
    ExternalWorkerLaunchRequest, ExternalWorkerProvider, ExternalWorkerRecord,
    ExternalWorkerRunRecord, ExternalWorkerState,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{IdempotencyStart, OrchestrationService};
use crate::event_bus::EventBus;
use crate::external_worker::{
    redact_external_detail, CursorCloudAdapter, ExternalWorkerAdapter, ExternalWorkerAdapterError,
    ExternalWorkerRegistry,
};
use crate::orchestration::authz::require_workspace_match;
use crate::orchestration::{
    hash_payload, merge_bounds, prompt_preview, reject_control_prompt, AuthContext,
    DurableExternalRun, DurableExternalWorker, ExternalRunAttachment, OrchError, OrchErrorCode,
    RunAggregates, RunRecord, RunState,
};

const MAX_RETAINED_EVENTS: usize = 256;
const EXTERNAL_CLIENT_ID: &str = "external_worker";

/// Install a Cursor Cloud adapter from trusted-host environment variables.
///
/// The API key is registered for journal redaction and never returned. An
/// adapter is only registered when both a key and a non-empty repository
/// allowlist are present; missing configuration leaves the registry empty.
pub fn external_worker_registry_from_env(bus: &EventBus) -> Arc<ExternalWorkerRegistry> {
    let registry = Arc::new(ExternalWorkerRegistry::new());
    let Some(api_key) = std::env::var("GROKPTAH_CURSOR_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return registry;
    };
    bus.add_control_secrets([api_key.clone()]);
    let allowlist = std::env::var("GROKPTAH_CURSOR_REPOSITORY_ALLOWLIST")
        .ok()
        .map(|value| {
            value
                .split(|ch: char| ch == ',' || ch.is_whitespace())
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if allowlist.is_empty() {
        eprintln!(
            "[grokptah] MCP control: GROKPTAH_CURSOR_API_KEY is set without GROKPTAH_CURSOR_REPOSITORY_ALLOWLIST; Cursor Cloud adapter not installed"
        );
        return registry;
    }
    let adapter = if let Ok(base) = std::env::var("GROKPTAH_CURSOR_API_BASE") {
        let base = base.trim().to_string();
        if base.is_empty() {
            CursorCloudAdapter::new(api_key)
        } else {
            CursorCloudAdapter::with_base_url(&base, api_key)
        }
    } else {
        CursorCloudAdapter::new(api_key)
    };
    match adapter.and_then(|adapter| adapter.with_repository_allowlist(allowlist)) {
        Ok(adapter) => registry.register(Arc::new(adapter)),
        Err(error) => {
            eprintln!("[grokptah] MCP control: Cursor Cloud adapter not installed ({error})");
        }
    }
    registry
}

impl OrchestrationService {
    /// Replace the process-local provider registry. Host setup and tests call
    /// this after construction; providers are never inferred from credentials
    /// alone.
    pub fn install_external_worker_registry(&self, registry: Arc<ExternalWorkerRegistry>) {
        *self.external_workers.lock() = registry;
    }

    pub fn external_worker_registry(&self) -> Arc<ExternalWorkerRegistry> {
        self.external_workers.lock().clone()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn launch_external_worker(
        &self,
        auth: &AuthContext,
        request: ExternalWorkerLaunchRequest,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<Value, OrchError> {
        let _ = auth;
        let tool = "ptah_launch_external_worker";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "request": request,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, error: OrchError| {
            svc.audit_err(
                tool,
                Some(&request.request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            error
        };

        if let Err(error) = reject_control_prompt(&request.prompt) {
            return Err(fail(self, error));
        }
        if let Err(error) = request.validate() {
            return Err(fail(
                self,
                OrchError::new(OrchErrorCode::InvalidRequest, error),
            ));
        }
        if request.auto_create_pr {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "pull-request creation requires a separate approval action",
                ),
            ));
        }
        if request.execution_mode != ExternalWorkerExecutionMode::Isolated {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "external workers must use isolated execution",
                ),
            ));
        }

        let claimed = match self.authorize_external_scope(session_id, workspace) {
            Ok(claimed) => claimed,
            Err(error) => return Err(fail(self, error)),
        };
        let ceiling = self.config.lock().bounds.clone();
        let bounds_json = request.bounds.as_ref().map(|bounds| {
            json!({
                "maxPromptBytes": bounds.max_prompt_bytes,
                "maxRounds": bounds.max_rounds,
                "maxDurationMs": bounds.max_duration_ms,
            })
        });
        let bounds = match merge_bounds(&ceiling, bounds_json.as_ref()) {
            Ok(bounds) => bounds,
            Err(error) => return Err(fail(self, error)),
        };
        if request.prompt.len() > bounds.max_prompt_bytes {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!(
                        "prompt exceeds max_prompt_bytes ({})",
                        bounds.max_prompt_bytes
                    ),
                ),
            ));
        }

        let mut lease = match self
            .begin_idempotency(tool, &request.request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        if let Err(error) = self.ensure_session_external_idle(session_id) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let adapter = match self.adapter_for(request.provider) {
            Ok(adapter) => adapter,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };

        let launched = match adapter.launch(&request).await {
            Ok(result) => result,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    adapter_error(error),
                ))
            }
        };
        if let Err(error) = launched.validate() {
            return Err(self.fail_claim(
                &mut lease,
                None,
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, error),
            ));
        }
        if launched.worker.repository != request.repository
            || launched.worker.starting_ref != request.starting_ref
        {
            return Err(self.fail_claim(
                &mut lease,
                None,
                session_id,
                &claimed,
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "provider did not retain the exact repository and starting ref",
                ),
            ));
        }

        let local_run_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let mut worker = launched.worker.clone();
        let mut run = redact_run(self, launched.run);
        if let Err(error) = self.persist_launch(
            &claimed,
            session_id,
            &local_run_id,
            &request,
            bounds,
            &mut worker,
            &mut run,
            now,
        ) {
            return Err(self.fail_claim(
                &mut lease,
                Some(local_run_id),
                session_id,
                &claimed,
                error,
            ));
        }
        let response = launch_projection(&worker, &run, &local_run_id, session_id, 1);
        if let Err(error) = lease.complete(Some(local_run_id.clone()), response.clone()) {
            return Err(self.fail_claim(
                &mut lease,
                Some(local_run_id),
                session_id,
                &claimed,
                error,
            ));
        }
        self.audit(
            tool,
            Some(&request.request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "external worker launch",
        );
        Ok(response)
    }

    pub fn list_external_workers_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<Value, OrchError> {
        let claimed = self.authorize_external_scope(session_id, workspace)?;
        let claimed_s = claimed.display().to_string();
        let workers = self
            .store
            .list_external_workers()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .filter(|worker| worker.session_id == session_id && worker.workspace == claimed_s)
            .map(|worker| {
                json!({
                    "worker": worker.worker,
                    "localRunId": worker.local_run_id,
                    "sessionId": worker.session_id,
                    "version": worker.version,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "workers": workers }))
    }

    pub async fn get_external_worker_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
    ) -> Result<Value, OrchError> {
        let (mut durable, adapter) =
            self.load_authorized_worker(session_id, workspace, external_agent_id)?;
        match adapter.get_worker(external_agent_id).await {
            Ok(worker) => {
                worker
                    .validate()
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error))?;
                if worker.repository != durable.worker.repository
                    || worker.starting_ref != durable.worker.starting_ref
                {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "provider identity no longer matches the exact repository and starting ref",
                    ));
                }
                if worker != durable.worker {
                    durable.version = durable.version.saturating_add(1);
                    durable.worker = worker;
                    self.store.save_external_worker(&durable).map_err(|error| {
                        OrchError::new(OrchErrorCode::Internal, error.to_string())
                    })?;
                }
            }
            Err(error) => return Err(adapter_error(error)),
        }
        Ok(json!({
            "worker": durable.worker,
            "localRunId": durable.local_run_id,
            "sessionId": durable.session_id,
            "version": durable.version,
        }))
    }

    pub async fn get_external_worker_run_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Value, OrchError> {
        let durable = self
            .reconnect_external_run(session_id, workspace, external_agent_id, external_run_id)
            .await?;
        Ok(run_projection(&durable))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn get_external_worker_events_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
        external_run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Value, OrchError> {
        if !(1..=500).contains(&limit) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "limit must be between 1 and 500",
            ));
        }
        let durable = self
            .reconnect_external_run(session_id, workspace, external_agent_id, external_run_id)
            .await?;
        event_page(&durable, after_seq, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn follow_up_external_worker(
        &self,
        auth: &AuthContext,
        request: ExternalWorkerFollowUpRequest,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
        expected_version: u64,
    ) -> Result<Value, OrchError> {
        let _ = auth;
        let tool = "ptah_follow_up_external_worker";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "externalAgentId": external_agent_id,
            "expectedVersion": expected_version,
            "request": request,
        });
        let phash = hash_payload(&payload);
        let claimed = self.authorize_external_scope(session_id, workspace)?;
        if let Err(error) = reject_control_prompt(&request.prompt) {
            return Err(self.fail_claim_pre(
                tool,
                &request.request_id,
                session_id,
                &claimed,
                error,
            ));
        }
        if let Err(error) = request.validate() {
            return Err(self.fail_claim_pre(
                tool,
                &request.request_id,
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::InvalidRequest, error),
            ));
        }

        let mut lease = match self
            .begin_idempotency(tool, &request.request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let (mut worker, adapter) =
            match self.load_authorized_worker(session_id, workspace, external_agent_id) {
                Ok(value) => value,
                Err(error) => {
                    return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
                }
            };
        if worker.version != expected_version {
            return Err(self.fail_claim(
                &mut lease,
                Some(worker.local_run_id.clone()),
                session_id,
                &claimed,
                stale_version(expected_version, worker.version),
            ));
        }
        if let Err(error) = self.ensure_worker_run_idle(external_agent_id) {
            return Err(self.fail_claim(
                &mut lease,
                Some(worker.local_run_id.clone()),
                session_id,
                &claimed,
                error,
            ));
        }
        if matches!(
            worker.worker.state,
            ExternalWorkerState::Unknown
                | ExternalWorkerState::Failed
                | ExternalWorkerState::Cancelled
                | ExternalWorkerState::Archived
        ) {
            return Err(self.fail_claim(
                &mut lease,
                Some(worker.local_run_id.clone()),
                session_id,
                &claimed,
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "external worker is not eligible for follow-up",
                ),
            ));
        }

        let follow = match adapter.follow_up(external_agent_id, &request).await {
            Ok(run) => redact_run(self, run),
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(worker.local_run_id.clone()),
                    session_id,
                    &claimed,
                    adapter_error(error),
                ))
            }
        };
        if let Err(error) = follow.validate() {
            return Err(self.fail_claim(
                &mut lease,
                Some(worker.local_run_id.clone()),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, error),
            ));
        }
        if follow.external_agent_id != external_agent_id {
            return Err(self.fail_claim(
                &mut lease,
                Some(worker.local_run_id.clone()),
                session_id,
                &claimed,
                OrchError::new(
                    OrchErrorCode::Internal,
                    "follow-up run is not attributed to the requested worker",
                ),
            ));
        }

        let local_run_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let start_seq = self.bus.next_seq();
        let parent_run_id = worker.local_run_id.clone();
        let run_record = RunRecord {
            run_id: local_run_id.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            request_id: request.request_id.clone(),
            client_id: Some(EXTERNAL_CLIENT_ID.into()),
            state: local_run_state(follow.state),
            agent_id: None,
            retry_of: None,
            parent_run_id: Some(parent_run_id),
            queue_position: None,
            bounds: self.config.lock().bounds.clone(),
            prompt_preview: self.bus.redact_text(&prompt_preview(&request.prompt), 500),
            start_seq: Some(start_seq),
            end_seq: terminal_end_seq(follow.state, start_seq),
            created_at: now,
            updated_at: now,
            terminal_result: follow
                .terminal_result
                .as_deref()
                .and_then(|value| redact_persisted_detail(self, value)),
            final_response: None,
            error_code: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
            external: Some(attachment_from(
                &worker.worker,
                &follow,
                &request.request_id,
            )),
        };
        if let Err(error) = self.store.save_run(&run_record) {
            return Err(self.fail_claim(
                &mut lease,
                Some(local_run_id),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, error.to_string()),
            ));
        }

        let mut durable_run = DurableExternalRun {
            run: follow.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            local_run_id: local_run_id.clone(),
            request_id: request.request_id.clone(),
            provider: worker.worker.provider,
            provider_id: worker.worker.provider_id.clone(),
            repository: worker.worker.repository.clone(),
            starting_ref: worker.worker.starting_ref.clone(),
            version: 1,
            stream_expired: false,
            events: Vec::new(),
            artifacts: Vec::new(),
        };
        append_event(
            self,
            &mut durable_run,
            "run.follow_up",
            "follow-up run accepted",
        );
        if let Err(error) = self.store.save_external_run(&durable_run) {
            return Err(self.fail_claim(
                &mut lease,
                Some(local_run_id),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, error.to_string()),
            ));
        }
        worker.version = worker.version.saturating_add(1);
        worker.local_run_id = local_run_id.clone();
        worker.worker.state = follow.state;
        worker.worker.updated_at = follow.updated_at.clone();
        if let Err(error) = self.store.save_external_worker(&worker) {
            return Err(self.fail_claim(
                &mut lease,
                Some(local_run_id),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, error.to_string()),
            ));
        }

        let response = run_projection(&durable_run);
        if let Err(error) = lease.complete(Some(local_run_id.clone()), response.clone()) {
            return Err(self.fail_claim(
                &mut lease,
                Some(local_run_id),
                session_id,
                &claimed,
                error,
            ));
        }
        self.audit(
            tool,
            Some(&request.request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "external worker follow-up",
        );
        Ok(response)
    }

    pub async fn list_external_worker_artifacts_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Value, OrchError> {
        let (mut durable, adapter) = self.load_authorized_run_pair(
            session_id,
            workspace,
            external_agent_id,
            external_run_id,
        )?;
        let artifacts = match adapter
            .list_artifacts(external_agent_id, external_run_id)
            .await
        {
            Ok(artifacts) => artifacts,
            Err(error) => return Err(adapter_error(error)),
        };
        for artifact in &artifacts {
            artifact
                .validate()
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error))?;
            if artifact.digest.trim().is_empty() {
                return Err(OrchError::new(
                    OrchErrorCode::Internal,
                    "external worker artifact listing did not provide a content digest",
                ));
            }
        }
        if durable.artifacts != artifacts {
            durable.artifacts = artifacts;
            durable.version = durable.version.saturating_add(1);
            self.store
                .save_external_run(&durable)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        Ok(json!({ "artifacts": durable.artifacts }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn cancel_external_worker(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
        external_run_id: &str,
        expected_version: u64,
    ) -> Result<Value, OrchError> {
        let _ = auth;
        let tool = "ptah_cancel_external_worker";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "externalAgentId": external_agent_id,
            "externalRunId": external_run_id,
            "expectedVersion": expected_version,
        });
        let phash = hash_payload(&payload);
        let claimed = self.authorize_external_scope(session_id, workspace)?;
        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (mut durable, adapter) = match self.load_authorized_run_pair(
            session_id,
            workspace,
            external_agent_id,
            external_run_id,
        ) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        if durable.version != expected_version {
            return Err(self.fail_claim(
                &mut lease,
                Some(durable.local_run_id.clone()),
                session_id,
                &claimed,
                stale_version(expected_version, durable.version),
            ));
        }
        if durable.run.state == ExternalWorkerState::Cancelled {
            let response = run_projection(&durable);
            if let Err(error) = lease.complete(Some(durable.local_run_id.clone()), response.clone())
            {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(durable.local_run_id.clone()),
                    session_id,
                    &claimed,
                    error,
                ));
            }
            return Ok(response);
        }
        if terminal_like(durable.run.state) {
            return Err(self.fail_claim(
                &mut lease,
                Some(durable.local_run_id.clone()),
                session_id,
                &claimed,
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "external worker run is already terminal and cannot be cancelled",
                ),
            ));
        }

        let cancelled = match adapter.cancel(external_agent_id, external_run_id).await {
            Ok(run) => redact_run(self, run),
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(durable.local_run_id.clone()),
                    session_id,
                    &claimed,
                    adapter_error(error),
                ))
            }
        };
        if cancelled.state != ExternalWorkerState::Cancelled {
            return Err(self.fail_claim(
                &mut lease,
                Some(durable.local_run_id.clone()),
                session_id,
                &claimed,
                OrchError::new(
                    OrchErrorCode::Internal,
                    "external worker cancellation was not terminal",
                ),
            ));
        }
        durable.run = cancelled;
        durable.version = durable.version.saturating_add(1);
        append_event(
            self,
            &mut durable,
            "run.cancelled",
            "run cancelled; cancellation is terminal",
        );
        if let Err(error) = self.persist_run_and_local(&durable) {
            return Err(self.fail_claim(
                &mut lease,
                Some(durable.local_run_id.clone()),
                session_id,
                &claimed,
                error,
            ));
        }
        if let Ok(Some(mut worker)) = self.store.load_external_worker(external_agent_id) {
            worker.version = worker.version.saturating_add(1);
            worker.worker.updated_at = durable.run.updated_at.clone();
            let _ = self.store.save_external_worker(&worker);
        }
        let response = run_projection(&durable);
        if let Err(error) = lease.complete(Some(durable.local_run_id.clone()), response.clone()) {
            return Err(self.fail_claim(
                &mut lease,
                Some(durable.local_run_id.clone()),
                session_id,
                &claimed,
                error,
            ));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "external worker cancel",
        );
        Ok(response)
    }

    async fn reconnect_external_run(
        &self,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<DurableExternalRun, OrchError> {
        let (mut durable, adapter) = self.load_authorized_run_pair(
            session_id,
            workspace,
            external_agent_id,
            external_run_id,
        )?;
        let after_seq = durable.run.last_seq;
        let mut changed = false;
        match adapter
            .try_stream_events(external_agent_id, external_run_id, after_seq)
            .await
        {
            Ok(Some(events)) => {
                for event in events {
                    event
                        .validate()
                        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error))?;
                    if let Some(detail) = redact_persisted_detail(self, &event.detail) {
                        let mut event = event;
                        event.detail = detail;
                        absorb_event(&mut durable, event);
                        changed = true;
                    }
                }
            }
            Ok(None) => {
                if !durable.stream_expired {
                    durable.stream_expired = true;
                    changed = true;
                    append_event(
                        self,
                        &mut durable,
                        "run.stream_expired",
                        "stream expired; polled status",
                    );
                }
            }
            Err(_) => {
                if !durable.stream_expired {
                    durable.stream_expired = true;
                    changed = true;
                    append_event(
                        self,
                        &mut durable,
                        "run.stream_expired",
                        "stream unavailable; polled status",
                    );
                }
            }
        }

        let polled = match adapter.get_run(external_agent_id, external_run_id).await {
            Ok(run) => redact_run(self, run),
            Err(error) => return Err(adapter_error(error)),
        };
        polled
            .validate()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error))?;
        if polled.external_agent_id != external_agent_id
            || polled.external_run_id != external_run_id
        {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "status poll is not attributed to the requested run",
            ));
        }
        let state_changed = polled.state != durable.run.state;
        if state_changed || polled.last_seq != durable.run.last_seq {
            changed = true;
        }
        let retained_last_seq = durable.run.last_seq.max(polled.last_seq);
        durable.run = polled;
        durable.run.last_seq = retained_last_seq;
        if state_changed {
            let status_detail = match durable.run.state {
                ExternalWorkerState::Completed => "run completed",
                ExternalWorkerState::Failed => "run failed",
                ExternalWorkerState::Cancelled => "run cancelled",
                ExternalWorkerState::Running => "run running",
                ExternalWorkerState::Provisioning => "run provisioning",
                _ => "status polled",
            };
            append_event(self, &mut durable, "run.status", status_detail);
        }
        if changed {
            durable.version = durable.version.saturating_add(1);
        }
        self.persist_run_and_local(&durable)?;
        Ok(durable)
    }

    fn authorize_external_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        require_workspace_match(&allowlist, cwd.as_deref(), workspace)
    }

    fn adapter_for(
        &self,
        provider: ExternalWorkerProvider,
    ) -> Result<Arc<dyn ExternalWorkerAdapter>, OrchError> {
        self.external_worker_registry()
            .get(provider)
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Unsupported,
                    "external worker provider is not installed",
                )
            })
    }

    fn load_authorized_worker(
        &self,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
    ) -> Result<(DurableExternalWorker, Arc<dyn ExternalWorkerAdapter>), OrchError> {
        let claimed = self.authorize_external_scope(session_id, workspace)?;
        let worker = self
            .store
            .load_external_worker(external_agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| {
                OrchError::new(OrchErrorCode::InvalidRequest, "unknown external worker")
            })?;
        if worker.session_id != session_id || worker.workspace != claimed.display().to_string() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "external worker does not belong to the requested session workspace",
            ));
        }
        let adapter = self.adapter_for(worker.worker.provider)?;
        Ok((worker, adapter))
    }

    fn load_authorized_run_pair(
        &self,
        session_id: Uuid,
        workspace: &Path,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<(DurableExternalRun, Arc<dyn ExternalWorkerAdapter>), OrchError> {
        let (worker, adapter) =
            self.load_authorized_worker(session_id, workspace, external_agent_id)?;
        let run = self
            .store
            .load_external_run(external_agent_id, external_run_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| {
                OrchError::new(OrchErrorCode::InvalidRequest, "unknown external worker run")
            })?;
        if run.session_id != worker.session_id || run.workspace != worker.workspace {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "external worker run does not belong to the requested session workspace",
            ));
        }
        Ok((run, adapter))
    }

    fn ensure_session_external_idle(&self, session_id: Uuid) -> Result<(), OrchError> {
        let runs = self
            .store
            .list_external_runs()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if runs.iter().any(|run| {
            run.session_id == session_id
                && matches!(
                    run.run.state,
                    ExternalWorkerState::Provisioning | ExternalWorkerState::Running
                )
        }) {
            return Err(OrchError::new(
                OrchErrorCode::SessionBusy,
                "session already has an active external worker run",
            ));
        }
        Ok(())
    }

    fn ensure_worker_run_idle(&self, external_agent_id: &str) -> Result<(), OrchError> {
        let runs = self
            .store
            .list_external_runs()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if runs.iter().any(|run| {
            run.run.external_agent_id == external_agent_id
                && matches!(
                    run.run.state,
                    ExternalWorkerState::Provisioning | ExternalWorkerState::Running
                )
        }) {
            return Err(OrchError::new(
                OrchErrorCode::SessionBusy,
                "external worker already has an active run",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_launch(
        &self,
        claimed: &Path,
        session_id: Uuid,
        local_run_id: &str,
        request: &ExternalWorkerLaunchRequest,
        bounds: crate::orchestration::RunBounds,
        worker: &mut ExternalWorkerRecord,
        run: &mut ExternalWorkerRunRecord,
        now: chrono::DateTime<Utc>,
    ) -> Result<(), OrchError> {
        let start_seq = self.bus.next_seq();
        let local = RunRecord {
            run_id: local_run_id.to_string(),
            session_id,
            workspace: claimed.display().to_string(),
            request_id: request.request_id.clone(),
            client_id: Some(EXTERNAL_CLIENT_ID.into()),
            state: local_run_state(run.state),
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: None,
            bounds,
            prompt_preview: self.bus.redact_text(&prompt_preview(&request.prompt), 500),
            start_seq: Some(start_seq),
            end_seq: terminal_end_seq(run.state, start_seq),
            created_at: now,
            updated_at: now,
            terminal_result: run
                .terminal_result
                .as_deref()
                .and_then(|value| redact_persisted_detail(self, value)),
            final_response: None,
            error_code: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
            external: Some(attachment_from(worker, run, &request.request_id)),
        };
        self.store
            .save_run(&local)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let durable_worker = DurableExternalWorker {
            worker: worker.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            local_run_id: local_run_id.to_string(),
            launch_request_id: request.request_id.clone(),
            version: 1,
        };
        self.store
            .save_external_worker(&durable_worker)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let mut durable_run = DurableExternalRun {
            run: run.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            local_run_id: local_run_id.to_string(),
            request_id: request.request_id.clone(),
            provider: worker.provider,
            provider_id: worker.provider_id.clone(),
            repository: worker.repository.clone(),
            starting_ref: worker.starting_ref.clone(),
            version: 1,
            stream_expired: false,
            events: Vec::new(),
            artifacts: Vec::new(),
        };
        append_event(
            self,
            &mut durable_run,
            "run.launched",
            "external worker launched",
        );
        self.store
            .save_external_run(&durable_run)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        *run = durable_run.run;
        Ok(())
    }

    fn persist_run_and_local(&self, durable: &DurableExternalRun) -> Result<(), OrchError> {
        self.store
            .save_external_run(durable)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let _ = self.store.update_run(&durable.local_run_id, |current| {
            current.state = local_run_state(durable.run.state);
            current.updated_at = Utc::now();
            current.terminal_result = durable.run.terminal_result.clone();
            if let Some(start_seq) = current.start_seq {
                current.end_seq = terminal_end_seq(durable.run.state, start_seq);
            }
            if let Some(external) = current.external.as_mut() {
                external.external_run_id = durable.run.external_run_id.clone();
            }
            Ok(())
        });
        Ok(())
    }

    fn fail_claim_pre(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        error: OrchError,
    ) -> OrchError {
        self.audit_err(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&workspace.display().to_string()),
            &error,
        );
        error
    }
}

fn local_run_state(state: ExternalWorkerState) -> RunState {
    match state {
        ExternalWorkerState::Provisioning
        | ExternalWorkerState::Ready
        | ExternalWorkerState::Running => RunState::Running,
        ExternalWorkerState::Completed | ExternalWorkerState::Archived => RunState::Completed,
        ExternalWorkerState::Failed | ExternalWorkerState::Unknown => RunState::Failed,
        ExternalWorkerState::Cancelled => RunState::Cancelled,
    }
}

fn terminal_end_seq(state: ExternalWorkerState, start_seq: u64) -> Option<u64> {
    terminal_like(state).then_some(start_seq)
}

fn terminal_like(state: ExternalWorkerState) -> bool {
    matches!(
        state,
        ExternalWorkerState::Completed
            | ExternalWorkerState::Failed
            | ExternalWorkerState::Cancelled
            | ExternalWorkerState::Archived
    )
}

fn attachment_from(
    worker: &ExternalWorkerRecord,
    run: &ExternalWorkerRunRecord,
    request_id: &str,
) -> ExternalRunAttachment {
    ExternalRunAttachment {
        provider: worker.provider,
        provider_id: worker.provider_id.clone(),
        external_agent_id: worker.external_agent_id.clone(),
        external_run_id: run.external_run_id.clone(),
        request_id: request_id.to_string(),
        repository: worker.repository.clone(),
        starting_ref: worker.starting_ref.clone(),
    }
}

fn launch_projection(
    worker: &ExternalWorkerRecord,
    run: &ExternalWorkerRunRecord,
    local_run_id: &str,
    session_id: Uuid,
    version: u64,
) -> Value {
    json!({
        "worker": worker,
        "run": run,
        "localRunId": local_run_id,
        "sessionId": session_id,
        "version": version,
    })
}

fn run_projection(durable: &DurableExternalRun) -> Value {
    json!({
        "run": durable.run,
        "localRunId": durable.local_run_id,
        "sessionId": durable.session_id,
        "version": durable.version,
        "streamExpired": durable.stream_expired,
        "lastSeq": durable.run.last_seq,
    })
}

fn event_page(
    durable: &DurableExternalRun,
    after_seq: u64,
    limit: usize,
) -> Result<Value, OrchError> {
    let retained_start = durable.events.first().map(|event| event.seq).unwrap_or(0);
    let retained_end = durable.events.last().map(|event| event.seq).unwrap_or(0);
    if after_seq > 0 && retained_start > 0 && after_seq < retained_start.saturating_sub(1) {
        let poll_route = format!(
            "/external-workers/{}/runs/{}",
            durable.run.external_agent_id, durable.run.external_run_id
        );
        return Err(OrchError::with_data(
            OrchErrorCode::CursorExpired,
            "external worker event cursor is below the retained window; resume from eventRange",
            json!({
                "eventRange": { "startSeq": retained_start, "endSeq": retained_end },
                "pollRoute": poll_route,
            }),
        ));
    }
    let mut events = durable
        .events
        .iter()
        .filter(|event| event.seq > after_seq)
        .cloned()
        .collect::<Vec<_>>();
    events.truncate(limit);
    let next_cursor = events.last().map(|event| event.seq);
    Ok(json!({
        "events": events,
        "nextCursor": next_cursor,
        "lastSeq": durable.run.last_seq,
        "pollRoute": format!(
            "/external-workers/{}/runs/{}",
            durable.run.external_agent_id, durable.run.external_run_id
        ),
    }))
}

fn append_event(
    svc: &OrchestrationService,
    durable: &mut DurableExternalRun,
    kind: &str,
    detail: &str,
) {
    let seq = durable
        .events
        .last()
        .map(|event| event.seq.saturating_add(1))
        .unwrap_or(1);
    let detail = redact_persisted_detail(svc, detail).unwrap_or_else(|| "status updated".into());
    absorb_event(
        durable,
        ExternalWorkerEvent {
            seq,
            ts: Utc::now().to_rfc3339(),
            kind: kind.into(),
            detail,
        },
    );
}

fn absorb_event(durable: &mut DurableExternalRun, event: ExternalWorkerEvent) {
    if durable
        .events
        .iter()
        .any(|existing| existing.seq == event.seq)
    {
        return;
    }
    durable.run.last_seq = durable.run.last_seq.max(event.seq);
    durable.events.push(event);
    durable.events.sort_by_key(|event| event.seq);
    if durable.events.len() > MAX_RETAINED_EVENTS {
        let drop = durable.events.len() - MAX_RETAINED_EVENTS;
        durable.events.drain(..drop);
    }
}

fn redact_run(
    svc: &OrchestrationService,
    mut run: ExternalWorkerRunRecord,
) -> ExternalWorkerRunRecord {
    if let Some(result) = run.terminal_result.take() {
        run.terminal_result = redact_persisted_detail(svc, &result);
    }
    run
}

fn redact_persisted_detail(svc: &OrchestrationService, value: &str) -> Option<String> {
    let redacted = svc.bus.redact_text(value, 4_096);
    if redacted.contains("[redacted]") {
        return None;
    }
    redact_external_detail(&redacted)
}

fn stale_version(expected: u64, current: u64) -> OrchError {
    OrchError::new(
        OrchErrorCode::StaleVersion,
        format!("stale external worker version: expected {expected}, current {current}"),
    )
}

fn adapter_error(error: ExternalWorkerAdapterError) -> OrchError {
    match error {
        ExternalWorkerAdapterError::InvalidRequest(message) => {
            let code = if message.contains("active run") {
                OrchErrorCode::SessionBusy
            } else {
                OrchErrorCode::InvalidRequest
            };
            OrchError::new(code, message)
        }
        ExternalWorkerAdapterError::UnsupportedProvider => OrchError::new(
            OrchErrorCode::Unsupported,
            "external worker provider is unsupported",
        ),
        ExternalWorkerAdapterError::InvalidBaseUrl => OrchError::new(
            OrchErrorCode::Unsupported,
            "external worker provider is unavailable",
        ),
        ExternalWorkerAdapterError::Provider { status } if status.as_u16() == 409 => {
            OrchError::new(
                OrchErrorCode::SessionBusy,
                "external worker provider reported a busy conflict",
            )
        }
        ExternalWorkerAdapterError::Provider { status } if status.as_u16() == 404 => {
            OrchError::new(OrchErrorCode::InvalidRequest, "unknown external worker")
        }
        ExternalWorkerAdapterError::Provider { .. }
        | ExternalWorkerAdapterError::InvalidResponse(_)
        | ExternalWorkerAdapterError::Transport(_) => OrchError::new(
            OrchErrorCode::Internal,
            "external worker provider request failed",
        ),
    }
}
