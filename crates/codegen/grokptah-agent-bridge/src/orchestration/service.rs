//! Orchestration service: reads + bounded mutations over AgentHostHandle (#196).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

use crate::event_bus::{CursorExpiredError, EventBus, EventReceiver, JournalPage};
use crate::host::AgentHostHandle;
use crate::prompt_queue::{PromptQueueEntry, SteeringDisposition};
use crate::session::{SessionKind, WorkspaceStatus};

use super::authz::{
    authenticate_bearer, canonical_workspace, require_workspace_match, AuthContext, AuthCredential,
    WorkspaceAllowlist,
};
use super::managed::{
    assemble_managed_run_input, managed_execution_eligible, select_relevant_managed_messages,
    ManagedExecutionIntent, ManagedExecutionPolicy, ManagedIntentState, ManagedRetryCause,
    NativeExecutorStatus, DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS, MANAGED_EXECUTION_SCHEMA_VERSION,
    MAX_MANAGED_MESSAGES,
};
use super::message::{message_activation_unsupported, MessageKind, WorkMessage};
use super::routine::{
    manual_dedupe_key, ActivationCause, ActivationRequest, MissedRunPolicy,
    RoutineConcurrencyPolicy, RoutineLifecycle, RoutineRecord, RoutineRetryPolicy, RoutineTrigger,
    WorkTemplate,
};
use super::store::{IdempotencyClaim, OrchStore};
use super::supervisor::{
    RoutineSupervisor, RoutineSupervisorStatus, WorkloadSupervisor, WorkloadSupervisorStatus,
    DEFAULT_ROUTINE_TICK_INTERVAL, DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL,
};
use super::types::*;
use super::worker::{reject_privilege_amplification, WorkerHostKind};
use super::workload::{
    WorkAttempt, WorkAttemptView, WorkDecision, WorkDependency, WorkItem, WorkPolicy, WorkProgress,
    WorkResult, WorkState,
};

/// Admission is deliberately bounded so an untrusted coordinator cannot turn
/// queued submissions into an unbounded in-memory prompt store.
const MAX_PENDING_ADMISSIONS: usize = 32;

#[derive(Default)]
struct AdmissionQueueState {
    pending: VecDeque<PendingRun>,
}

struct PendingRun {
    run_id: String,
    session_id: Uuid,
    prompt: String,
    execution_mode: RunExecutionMode,
}

#[derive(Clone)]
pub struct OrchestrationConfig {
    pub bearer_token: String,
    pub allowlist: WorkspaceAllowlist,
    pub max_concurrent_runs: usize,
    pub bounds: RunBounds,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            bearer_token: String::new(),
            allowlist: WorkspaceAllowlist::default(),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        }
    }
}

pub struct OrchestrationService {
    host: AgentHostHandle,
    bus: EventBus,
    store: OrchStore,
    config: Mutex<OrchestrationConfig>,
    auth_credentials: Mutex<Vec<AuthCredential>>,
    agent_owner_id: Mutex<String>,
    self_ref: Weak<OrchestrationService>,
    pending_admissions: Mutex<AdmissionQueueState>,
    scheduler_watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    workload_supervisor: Mutex<Option<WorkloadSupervisor>>,
    routine_supervisor: Mutex<Option<RoutineSupervisor>>,
    native_executor: Mutex<NativeExecutorStatus>,
    native_executor_watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Join handles for in-flight runs (prevents forget + unbounded leaks).
    join_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// Authorized bounds for a live run event stream.
#[derive(Debug, Clone)]
pub(crate) struct LiveRunScope {
    pub session_id: Uuid,
    pub run_id: String,
    pub start_seq: u64,
    pub end_seq: Option<u64>,
}

impl Drop for OrchestrationService {
    fn drop(&mut self) {
        if let Some(watcher) = self.scheduler_watcher.get_mut().take() {
            watcher.abort();
        }
        let pending = self
            .pending_admissions
            .get_mut()
            .pending
            .iter()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        for run_id in pending {
            self.host.release_orchestration_queue_slot(&run_id);
        }
    }
}

struct AdmissionGuard {
    host: AgentHostHandle,
    run_id: String,
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        self.host.release_orchestration_turn(&self.run_id);
    }
}

enum IdempotencyStart {
    Perform(IdempotencyLease),
    Replay(serde_json::Value),
}

/// Coordinator vs worker identities for an assignment mutation.
/// The authenticated principal remains `AuthContext.token_id`; these are
/// durable Agent resources the principal is authorized to name.
struct ScopedAssignment {
    worker: AgentRecord,
    manager: Option<AgentRecord>,
}

fn redact_claim_lease_token(mut response: serde_json::Value) -> serde_json::Value {
    if let Some(object) = response.as_object_mut() {
        object.remove("leaseToken");
    }
    response
}

struct IdempotencyLease {
    store: OrchStore,
    tool: String,
    request_id: String,
    payload_hash: String,
    settled: bool,
}

impl IdempotencyLease {
    fn complete(
        &mut self,
        run_id: Option<String>,
        response: serde_json::Value,
    ) -> Result<(), OrchError> {
        self.store.complete_idempotency(
            &self.tool,
            &self.request_id,
            &self.payload_hash,
            run_id,
            response,
        )?;
        self.settled = true;
        Ok(())
    }

    fn fail(&mut self, run_id: Option<String>, error: OrchError) -> OrchError {
        match self.store.fail_idempotency(
            &self.tool,
            &self.request_id,
            &self.payload_hash,
            run_id,
            error.clone(),
        ) {
            Ok(()) => {
                self.settled = true;
                error
            }
            Err(store_error) => store_error,
        }
    }
}

impl Drop for IdempotencyLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let error = OrchError::new(
            OrchErrorCode::Internal,
            "mutation abandoned before its durable outcome completed",
        );
        if self
            .store
            .fail_idempotency(
                &self.tool,
                &self.request_id,
                &self.payload_hash,
                None,
                error,
            )
            .is_ok()
        {
            self.settled = true;
        }
    }
}

impl OrchestrationService {
    pub fn new(
        host: AgentHostHandle,
        bus: EventBus,
        store: OrchStore,
        mut config: OrchestrationConfig,
    ) -> Arc<Self> {
        host.install_orchestration_store(store.clone());
        // The host owns the process-wide ledger. If desktop bootstrap opened
        // it first, use that same handle instead of creating a split history.
        let store = host.ensure_orchestration_store().unwrap_or(store);
        // Register control bearer (and any future secrets) on the *shared* host bus
        // so durable journal redaction covers the shipped desktop path.
        if !config.bearer_token.is_empty() {
            bus.add_control_secrets([config.bearer_token.clone()]);
        }
        config.max_concurrent_runs =
            host.configure_orchestration_capacity(config.max_concurrent_runs);
        let auth_credentials = if config.bearer_token.is_empty() {
            Vec::new()
        } else {
            vec![AuthCredential::new("primary", config.bearer_token.clone())
                .expect("non-empty bearer token should form a primary credential")]
        };
        let workload_supervisor =
            WorkloadSupervisor::start(store.clone(), DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL);
        let routine_supervisor =
            RoutineSupervisor::start(store.clone(), DEFAULT_ROUTINE_TICK_INTERVAL);
        let service = Arc::new_cyclic(|self_ref| Self {
            host,
            bus,
            store,
            config: Mutex::new(config),
            auth_credentials: Mutex::new(auth_credentials),
            agent_owner_id: Mutex::new("primary".into()),
            self_ref: self_ref.clone(),
            pending_admissions: Mutex::new(AdmissionQueueState::default()),
            scheduler_watcher: Mutex::new(None),
            workload_supervisor: Mutex::new(workload_supervisor),
            routine_supervisor: Mutex::new(routine_supervisor),
            native_executor: Mutex::new(NativeExecutorStatus::disabled(
                DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS,
            )),
            native_executor_watcher: Mutex::new(None),
            join_handles: Mutex::new(Vec::new()),
        });
        service.start_scheduler_watcher();
        service.start_native_executor();
        service
    }

    fn start_scheduler_watcher(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut events = self.host.subscribe_events();
        let wakeup = self.host.orchestration_wakeup();
        let service_ref = self.self_ref.clone();
        let watcher = runtime.spawn(async move {
            loop {
                tokio::select! {
                    update = events.recv() => {
                        let Some(update) = update else {
                            break;
                        };
                        if matches!(
                            update,
                            crate::events::SessionUpdate::TurnComplete { .. }
                                | crate::events::SessionUpdate::Error { .. }
                        ) {
                            let Some(service) = service_ref.upgrade() else {
                                break;
                            };
                            service.pump_pending();
                        }
                    }
                    _ = wakeup.notified() => {
                        let Some(service) = service_ref.upgrade() else {
                            break;
                        };
                        service.pump_pending();
                    }
                }
            }
        });
        *self.scheduler_watcher.lock() = Some(watcher);
    }

    fn start_native_executor(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        {
            let mut status = self.native_executor.lock();
            status.enabled = true;
            status.interval_ms = DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS;
            status.started_at = Some(Utc::now());
        }
        let service_ref = self.self_ref.clone();
        let mut events = self.host.subscribe_events();
        let watcher = runtime.spawn(async move {
            let mut ticker =
                tokio::time::interval(Duration::from_millis(DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS));
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let Some(service) = service_ref.upgrade() else { break; };
                        service.drive_native_executor_once().await;
                    }
                    update = events.recv() => {
                        let Some(update) = update else { break; };
                        let Some(service) = service_ref.upgrade() else { break; };
                        service.handle_native_executor_event(&update).await;
                    }
                }
            }
        });
        *self.native_executor_watcher.lock() = Some(watcher);
    }

    pub fn native_executor_status(&self) -> NativeExecutorStatus {
        self.native_executor.lock().clone()
    }

    pub async fn drive_native_executor_once(&self) {
        let now = Utc::now();
        self.native_executor.lock().last_tick_at = Some(now);
        if let Err(error) = self.recover_and_finalize_managed_intents().await {
            self.native_executor.lock().last_error = Some(error.to_string());
            return;
        }
        match self.admit_eligible_managed_work().await {
            Ok(()) => {
                let mut status = self.native_executor.lock();
                status.last_success_at = Some(now);
                status.last_error = None;
            }
            Err(error) => {
                self.native_executor.lock().last_error = Some(error.to_string());
            }
        }
    }

    async fn handle_native_executor_event(&self, update: &crate::events::SessionUpdate) {
        let crate::events::SessionUpdate::PermissionRequired {
            session_id,
            request,
        } = update
        else {
            return;
        };
        let Ok(intents) = self.store.list_managed_intents() else {
            return;
        };
        let Some(mut intent) = intents.into_iter().find(|intent| {
            intent.session_id == *session_id
                && matches!(
                    intent.state,
                    ManagedIntentState::Admitted | ManagedIntentState::Parked
                )
        }) else {
            return;
        };
        let (Some(attempt_id), Some(run_id)) = (&intent.attempt_id, &intent.run_id) else {
            return;
        };
        let secret = self.config.lock().bearer_token.clone();
        let Ok(Some(attempt)) = self.store.load_work_attempt(attempt_id) else {
            return;
        };
        let token = attempt.lease_token_for_secret(&secret);
        let reason = format!("permission required: {}", request.tool_name);
        if self
            .store
            .park_work_input(&intent.work_id, attempt_id, &token, &reason)
            .is_ok()
        {
            intent.state = ManagedIntentState::Parked;
            intent.permission_request_id = Some(request.id.to_string());
            intent.updated_at = Utc::now();
            let _ = self.store.save_managed_intent(&intent);
            let _ = self.store.send_message(
                super::message::WorkMessage::new(
                    MessageKind::Question,
                    "native-executor",
                    Some(intent.agent_id.clone()),
                    None,
                    intent.session_id,
                    intent.workspace.clone(),
                    Some(intent.work_id.clone()),
                    reason,
                    Some(json!({
                        "permissionId": request.id,
                        "toolName": request.tool_name,
                        "runId": run_id,
                    })),
                    Utc::now(),
                )
                .unwrap_or_else(|_| {
                    super::message::WorkMessage::new(
                        MessageKind::Informational,
                        "native-executor",
                        None,
                        None,
                        intent.session_id,
                        intent.workspace.clone(),
                        Some(intent.work_id.clone()),
                        "permission required",
                        None,
                        Utc::now(),
                    )
                    .expect("informational fallback")
                }),
            );
        }
    }

    async fn recover_and_finalize_managed_intents(&self) -> Result<(), OrchError> {
        let intents = self.store.list_managed_intents()?;
        let secret = self.config.lock().bearer_token.clone();
        for intent in intents {
            match intent.state {
                ManagedIntentState::Claiming => {
                    let recovered = self.store.reconcile_claiming_intent(
                        &intent.intent_id,
                        &secret,
                        Utc::now(),
                    )?;
                    if let Some(recovered) = recovered {
                        if recovered.state == ManagedIntentState::Admitted {
                            self.finalize_or_heartbeat_intent(&recovered, &secret)
                                .await?;
                        }
                    }
                }
                ManagedIntentState::Admitted | ManagedIntentState::Parked => {
                    self.finalize_or_heartbeat_intent(&intent, &secret).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn finalize_or_heartbeat_intent(
        &self,
        intent: &ManagedExecutionIntent,
        secret: &str,
    ) -> Result<(), OrchError> {
        let (Some(attempt_id), Some(run_id)) = (&intent.attempt_id, &intent.run_id) else {
            let _ = self
                .store
                .abandon_managed_intent(&intent.intent_id, Utc::now());
            return Ok(());
        };
        let Some(attempt) = self
            .store
            .load_work_attempt(attempt_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        else {
            return Ok(());
        };
        let token = attempt.lease_token_for_secret(secret);
        let Some(run) = self
            .store
            .load_run(run_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        else {
            return Ok(());
        };
        if run.state == RunState::Interrupted {
            let retry_eligible = self.managed_retry_eligible(&intent.agent_id);
            let closed = self.store.close_managed_attempt(
                &intent.intent_id,
                retry_eligible,
                ManagedRetryCause::Interrupted,
                "managed run interrupted; native executor does not resume the invocation",
                Utc::now(),
            )?;
            if closed.is_some() {
                self.native_executor.lock().finalized += 1;
            }
            return Ok(());
        }
        if !run.state.is_terminal() {
            let _ = self
                .store
                .renew_work_lease(&intent.work_id, attempt_id, &token, None);
            return Ok(());
        }
        let summary = run
            .final_response
            .clone()
            .or(run.terminal_result.clone())
            .unwrap_or_else(|| format!("{:?}", run.state));
        let result = WorkResult {
            summary,
            evidence: Vec::new(),
            artifacts: Vec::new(),
            failure: run.error_code.clone(),
            cancellation_reason: None,
            completed_at: Utc::now(),
        };
        let outcome = match run.state {
            RunState::Completed => self
                .store
                .complete_work(&intent.work_id, attempt_id, &token, result)
                .map(|_| ()),
            RunState::Cancelled => self
                .store
                .cancel_work_checked(&intent.work_id, "managed run cancelled", None)
                .map(|_| ()),
            _ => {
                let retry_eligible = self.managed_retry_eligible(&intent.agent_id);
                self.store
                    .close_managed_attempt(
                        &intent.intent_id,
                        retry_eligible,
                        ManagedRetryCause::Failed,
                        result.failure.as_deref().unwrap_or("managed run failed"),
                        Utc::now(),
                    )
                    .map(|_| ())
            }
        };
        if outcome.is_ok() {
            if run.state == RunState::Completed || run.state == RunState::Cancelled {
                let mut done = intent.clone();
                done.state = ManagedIntentState::Finalized;
                done.updated_at = Utc::now();
                let _ = self.store.save_managed_intent(&done);
            }
            self.native_executor.lock().finalized += 1;
        }
        Ok(())
    }

    fn managed_retry_eligible(&self, agent_id: &str) -> bool {
        self.store
            .load_agent(agent_id)
            .ok()
            .flatten()
            .and_then(|agent| agent.spec)
            .is_some_and(|spec| spec.managed_execution.retry_eligible)
    }

    async fn admit_eligible_managed_work(&self) -> Result<(), OrchError> {
        let items = self
            .store
            .list_work_items()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let ceiling = self.config.lock().bounds.clone();
        let owner = self.agent_owner_id.lock().clone();
        let secret = self.config.lock().bearer_token.clone();
        for work in items {
            if work.state != WorkState::Queued {
                continue;
            }
            if self
                .store
                .live_managed_intent_for_work(&work.work_id)?
                .is_some()
            {
                continue;
            }
            let Some(agent_id) = work.assigned_agent_id.clone() else {
                self.native_executor.lock().skipped_manual += 1;
                continue;
            };
            let Ok(Some(agent)) = self.store.load_agent(&agent_id) else {
                self.native_executor.lock().skipped_ineligible += 1;
                continue;
            };
            let Ok(spec) = agent.current_spec().cloned() else {
                self.native_executor.lock().skipped_ineligible += 1;
                continue;
            };
            if !spec.managed_execution.enabled {
                self.native_executor.lock().skipped_manual += 1;
                continue;
            }
            if work.attempt_count >= work.policy.retry.max_attempts {
                self.native_executor.lock().skipped_ineligible += 1;
                continue;
            }
            if work.attempt_count >= 1 && !spec.managed_execution.retry_eligible {
                let _ = self.store.seal_queued_managed_work(
                    &work.work_id,
                    "managed retryEligible forbids another native attempt",
                    Utc::now(),
                );
                self.native_executor.lock().skipped_ineligible += 1;
                continue;
            }
            let decisions = self.store.list_work_decisions(&work.work_id)?;
            let live = self.store.live_managed_intents_for_agent(&agent_id)?;
            let bounds = match managed_execution_eligible(
                &work, &agent, &spec, &decisions, live, &ceiling,
            ) {
                Ok(bounds) => bounds,
                Err(_) => {
                    self.native_executor.lock().skipped_ineligible += 1;
                    continue;
                }
            };
            if let Err(error) = self
                .admit_one_managed_work(&work, &agent, &spec, bounds, &owner, &secret)
                .await
            {
                let mut status = self.native_executor.lock();
                status.last_error = Some(error.to_string());
                status.skipped_ineligible += 1;
                continue;
            }
            self.native_executor.lock().admitted += 1;
        }
        Ok(())
    }

    async fn admit_one_managed_work(
        &self,
        work: &WorkItem,
        agent: &super::types::AgentRecord,
        spec: &super::types::AgentSpec,
        bounds: super::types::RunBounds,
        owner_id: &str,
        secret: &str,
    ) -> Result<(), OrchError> {
        let now = Utc::now();
        let parent = match &work.parent_work_id {
            Some(parent_id) => self
                .store
                .load_work_item(parent_id)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            None => None,
        };
        let page = self
            .store
            .list_messages(work.session_id, &work.workspace, 0, None, None, 200)
            .unwrap_or_else(|_| super::message::MessagePage {
                messages: Vec::new(),
                next_seq: 0,
                retained_from_seq: 1,
            });
        let messages = select_relevant_managed_messages(
            &page.messages,
            work,
            &agent.agent_id,
            Utc::now(),
            MAX_MANAGED_MESSAGES,
        );
        let (prompt, input_hash) = assemble_managed_run_input(
            work,
            spec,
            &bounds,
            work.attempt_count + 1,
            parent.as_ref(),
            &messages,
            None,
        )?;
        let mut intent = ManagedExecutionIntent {
            schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
            intent_id: Uuid::new_v4().to_string(),
            agent_id: agent.agent_id.clone(),
            agent_spec_revision: spec.revision,
            work_id: work.work_id.clone(),
            work_revision: work.revision,
            attempt_id: None,
            run_id: None,
            session_id: work.session_id,
            workspace: work.workspace.clone(),
            source_routine_id: work.source_routine_id.clone(),
            source_activation_id: work.source_activation_id.clone(),
            model_selection_key: spec.model.selection_key.clone(),
            bounds: bounds.clone(),
            input_hash,
            state: ManagedIntentState::Claiming,
            permission_request_id: None,
            created_at: now,
            updated_at: now,
        };
        self.store.save_managed_intent(&intent)?;
        let claim = match self.store.claim_work_with_lease_secret(
            &work.work_id,
            &agent.agent_id,
            None,
            secret,
        ) {
            Ok(claim) => claim,
            Err(error) => {
                let _ = self
                    .store
                    .abandon_managed_intent(&intent.intent_id, Utc::now());
                return Err(error);
            }
        };
        intent.attempt_id = Some(claim.attempt.attempt_id.clone());
        intent.updated_at = Utc::now();
        self.store.save_managed_intent(&intent)?;
        let auth = AuthContext {
            token_id: "native-executor".into(),
            owner_id: owner_id.to_string(),
        };
        let bounds_json = serde_json::to_value(&bounds)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let submitted = match self
            .submit_task_with_execution_mode_and_queue_parent(
                &auth,
                &intent.intent_id,
                work.session_id,
                Path::new(&work.workspace),
                prompt,
                Some(bounds_json),
                RunExecutionMode::Shared,
                false,
                None,
                "ptah_native_execute",
            )
            .await
        {
            Ok(value) => value,
            Err(error) => {
                if self
                    .store
                    .find_run_by_request_id(&intent.intent_id)?
                    .is_some()
                {
                    self.store
                        .reconcile_claiming_intent(&intent.intent_id, secret, Utc::now())?;
                    return Ok(());
                }
                let _ = self.store.release_work(
                    &work.work_id,
                    &claim.attempt.attempt_id,
                    &claim.lease_token,
                    "managed run admission failed",
                );
                let _ = self
                    .store
                    .abandon_managed_intent(&intent.intent_id, Utc::now());
                return Err(error);
            }
        };
        let run_id = submitted["runId"]
            .as_str()
            .ok_or_else(|| OrchError::new(OrchErrorCode::Internal, "managed run missing run_id"))?
            .to_string();
        self.store.link_work_run(
            &work.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            &run_id,
        )?;
        let _ = self.store.report_work_progress(
            &work.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            WorkProgress {
                summary: "native executor admitted a finite Run".into(),
                percent: Some(1),
                updated_at: Utc::now(),
            },
        );
        intent.run_id = Some(run_id);
        intent.state = ManagedIntentState::Admitted;
        intent.updated_at = Utc::now();
        self.store.save_managed_intent(&intent)?;
        Ok(())
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Stop background recovery before a caller reopens the shared ledger.
    /// This is separate from `Drop` because an async service shutdown must
    /// wait for the supervisor task to release its store handle.
    pub async fn stop_background_tasks(&self) {
        let supervisor = self.workload_supervisor.lock().take();
        if let Some(mut supervisor) = supervisor {
            supervisor.stop_and_wait();
        }
        let supervisor = self.routine_supervisor.lock().take();
        if let Some(mut supervisor) = supervisor {
            supervisor.stop_and_wait();
        }
        if let Some(watcher) = self.native_executor_watcher.lock().take() {
            watcher.abort();
        }
        self.native_executor.lock().enabled = false;
    }

    pub fn store(&self) -> &OrchStore {
        &self.store
    }

    pub fn set_token(&self, token: String) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            self.bus.add_control_secrets([token.clone()]);
        }
        self.config.lock().bearer_token = token.clone();
        let credentials = if token.is_empty() {
            Vec::new()
        } else {
            vec![AuthCredential::new("primary", token)
                .expect("non-empty bearer token should form a primary credential")]
        };
        *self.auth_credentials.lock() = credentials;
    }

    /// Install named device/client credentials while retaining the existing
    /// primary-token configuration field for compatibility with embedders.
    pub fn set_auth_credentials(&self, credentials: Vec<AuthCredential>) -> Result<(), OrchError> {
        if credentials.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "at least one auth credential is required",
            ));
        }
        if !credentials
            .iter()
            .any(|credential| credential.id == "primary")
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "auth credentials must include the primary credential",
            ));
        }
        let primary_token = credentials
            .iter()
            .find(|credential| credential.id == "primary")
            .expect("primary credential was checked above")
            .token()
            .to_string();
        for credential in &credentials {
            self.bus
                .add_control_secrets([credential.token().to_string()]);
        }
        self.config.lock().bearer_token = primary_token;
        *self.auth_credentials.lock() = credentials;
        Ok(())
    }

    pub fn set_agent_owner_id(&self, owner_id: String) -> Result<(), OrchError> {
        let owner_id = owner_id.trim().to_string();
        if owner_id.is_empty() || owner_id.len() > 128 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "Agent owner id must be between 1 and 128 bytes",
            ));
        }
        *self.agent_owner_id.lock() = owner_id;
        Ok(())
    }

    fn agent_owner_id(&self) -> String {
        self.agent_owner_id.lock().clone()
    }

    pub fn set_allowlist(&self, allowlist: WorkspaceAllowlist) {
        self.config.lock().allowlist = allowlist;
    }

    pub(crate) fn audit_transport_result(&self, tool: &str, error: Option<&OrchError>) {
        self.audit(
            tool,
            None,
            None,
            None,
            if error.is_some() {
                "rejected"
            } else {
                "accepted"
            },
            error.map(|e| e.code.as_str()),
            "mcp transport call",
        );
    }

    pub fn auth_header(&self, header: Option<&str>) -> Result<AuthContext, OrchError> {
        let credentials = self.auth_credentials.lock().clone();
        let owner_id = self.agent_owner_id();
        let res = authenticate_bearer(header, &credentials, &owner_id);
        if let Err(ref e) = res {
            self.audit(
                "auth",
                None,
                None,
                None,
                "rejected",
                Some(e.code.as_str()),
                "auth failed",
            );
        }
        res
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        tool: &str,
        request_id: Option<&str>,
        session_id: Option<Uuid>,
        workspace: Option<&str>,
        outcome: &str,
        error_code: Option<&str>,
        detail: &str,
    ) {
        let entry = AuditEntry {
            ts: Utc::now(),
            tool: self.bus.redact_text(tool, 100),
            request_id: request_id.map(|value| self.bus.redact_text(value, 256)),
            session_id,
            workspace: workspace.map(|value| self.bus.redact_text(value, 1_000)),
            outcome: self.bus.redact_text(outcome, 100),
            error_code: error_code.map(|value| self.bus.redact_text(value, 100)),
            detail: self.bus.redact_text(detail, 500),
        };
        if let Err(e) = self.store.enqueue_audit(entry) {
            eprintln!("[grokptah] orchestration audit persistence failed: {e}");
        }
    }

    fn audit_err(
        &self,
        tool: &str,
        request_id: Option<&str>,
        session_id: Option<Uuid>,
        workspace: Option<&str>,
        e: &OrchError,
    ) {
        self.audit(
            tool,
            request_id,
            session_id,
            workspace,
            "rejected",
            Some(e.code.as_str()),
            &e.message,
        );
    }

    fn try_reserve_capacity(&self, run_id: &str, session_id: Uuid) -> Result<(), OrchError> {
        self.host
            .reserve_orchestration_turn(run_id, session_id)
            .map_err(|error| {
                let message = error.to_string();
                if message.contains("max concurrent") {
                    OrchError::new(OrchErrorCode::CapacityExhausted, message)
                } else {
                    OrchError::new(OrchErrorCode::SessionBusy, message)
                }
            })
    }

    fn release_capacity(&self, run_id: &str) {
        self.host.release_orchestration_turn(run_id);
        self.pump_pending();
    }

    /// Keep the durable records aligned with the host-global scheduler. The
    /// prompt itself remains in memory by design; this metadata is only for
    /// honest operator/coordinator visibility while the run is pending.
    fn sync_pending_positions(&self) {
        let run_ids: Vec<String> = {
            let queue = self.pending_admissions.lock();
            queue
                .pending
                .iter()
                .map(|pending| pending.run_id.clone())
                .collect()
        };
        for run_id in run_ids {
            let Some(position) = self.host.orchestration_pending_position(&run_id) else {
                continue;
            };
            if let Err(error) = self.store.update_run(&run_id, |run| {
                if run.state == RunState::Queued {
                    run.queue_position = Some(position);
                    run.updated_at = Utc::now();
                }
                Ok(())
            }) {
                eprintln!("[grokptah] queued run position persistence failed: {error}");
            }
        }
    }

    fn clear_queue_position(&self, run_id: &str) {
        if let Err(error) = self.store.update_run(run_id, |run| {
            if run.queue_position.take().is_some() {
                run.updated_at = Utc::now();
            }
            Ok(())
        }) {
            eprintln!("[grokptah] queued run position clear failed: {error}");
        }
    }

    fn enqueue_pending(&self, pending: PendingRun) -> Result<usize, OrchError> {
        let mut queue = self.pending_admissions.lock();
        if queue.pending.len() >= MAX_PENDING_ADMISSIONS {
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                format!("bounded admission queue is full ({MAX_PENDING_ADMISSIONS} pending runs)"),
            ));
        }
        let run_id = pending.run_id.clone();
        self.host
            .reserve_orchestration_queue_slot(&run_id, pending.session_id)
            .map_err(|error| OrchError::new(OrchErrorCode::CapacityExhausted, error.to_string()))?;
        queue.pending.push_back(pending);
        drop(queue);
        self.sync_pending_positions();
        Ok(self
            .host
            .orchestration_pending_position(&run_id)
            .unwrap_or(1))
    }

    fn remove_pending(&self, run_id: &str) -> bool {
        let mut queue = self.pending_admissions.lock();
        let before = queue.pending.len();
        queue.pending.retain(|pending| pending.run_id != run_id);
        let removed = before != queue.pending.len();
        drop(queue);
        if removed {
            self.host.release_orchestration_queue_slot(run_id);
            self.sync_pending_positions();
        }
        removed
    }

    /// Promote as many queued tasks as the shared host capacity allows. The
    /// host atomically chooses the globally fair run and reserves its active
    /// turn, so two embedded control services cannot both select conflicting
    /// queue heads.
    fn pump_pending(&self) {
        loop {
            if self.host.orchestration_active_count() >= self.host.orchestration_capacity_limit() {
                return;
            }
            let candidates: Vec<(String, Uuid)> = {
                let queue = self.pending_admissions.lock();
                queue
                    .pending
                    .iter()
                    .map(|pending| (pending.run_id.clone(), pending.session_id))
                    .collect()
            };
            let Some((run_id, _session_id)) =
                candidates.into_iter().find(|(run_id, session_id)| {
                    self.host.claim_orchestration_pending(run_id, *session_id)
                })
            else {
                return;
            };
            let pending = {
                let mut queue = self.pending_admissions.lock();
                let Some(index) = queue.pending.iter().position(|p| p.run_id == run_id) else {
                    self.host.release_orchestration_turn(&run_id);
                    continue;
                };
                queue.pending.remove(index).expect("pending index exists")
            };
            self.clear_queue_position(&pending.run_id);
            self.sync_pending_positions();

            // Cancellation can win after the task left the queue but before
            // promotion. Treat terminal records as a normal, safe skip.
            let Some(current) = self.store.load_run(&pending.run_id).ok().flatten() else {
                self.host.release_orchestration_turn(&pending.run_id);
                continue;
            };
            if current.state != RunState::Queued {
                self.host.release_orchestration_turn(&pending.run_id);
                continue;
            }

            let start_seq = self.bus.next_seq();
            let transitioned = self.store.update_run(&pending.run_id, |run| {
                if run.state != RunState::Queued {
                    anyhow::bail!("queued run is no longer pending");
                }
                run.state = RunState::Running;
                run.queue_position = None;
                run.start_seq = Some(start_seq);
                run.updated_at = Utc::now();
                Ok(())
            });
            match transitioned {
                Ok(Some(run)) => self.spawn_run(run, pending.prompt, pending.execution_mode),
                Ok(None) | Err(_) => {
                    self.host.release_orchestration_turn(&pending.run_id);
                    if let Err(error) = self
                        .host
                        .reserve_orchestration_queue_slot(&pending.run_id, pending.session_id)
                    {
                        eprintln!("[grokptah] queued run could not be re-registered: {error}");
                    } else {
                        let mut queue = self.pending_admissions.lock();
                        queue.pending.push_front(pending);
                        drop(queue);
                        self.sync_pending_positions();
                        return;
                    }
                }
            }
        }
    }

    async fn begin_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<IdempotencyStart, OrchError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match self.store.claim_idempotency(tool, request_id, payload_hash) {
                Ok(IdempotencyClaim::Perform) => {
                    return Ok(IdempotencyStart::Perform(IdempotencyLease {
                        store: self.store.clone(),
                        tool: tool.into(),
                        request_id: request_id.into(),
                        payload_hash: payload_hash.into(),
                        settled: false,
                    }));
                }
                Ok(IdempotencyClaim::Replay(Ok(value))) => {
                    self.audit(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        "replayed",
                        None,
                        "replayed successful mutation outcome",
                    );
                    return Ok(IdempotencyStart::Replay(value));
                }
                Ok(IdempotencyClaim::Replay(Err(error))) => {
                    self.audit(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        "replayed",
                        Some(error.code.as_str()),
                        "replayed rejected mutation outcome",
                    );
                    return Err(error);
                }
                Ok(IdempotencyClaim::Pending) => {
                    if tokio::time::Instant::now() >= deadline {
                        let error = OrchError::new(
                            OrchErrorCode::Conflict,
                            "matching request_id is still in progress",
                        );
                        self.audit_err(
                            tool,
                            Some(request_id),
                            Some(session_id),
                            Some(&workspace.display().to_string()),
                            &error,
                        );
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => {
                    self.audit_err(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        &error,
                    );
                    return Err(error);
                }
            }
        }
    }

    fn fail_claim(
        &self,
        lease: &mut IdempotencyLease,
        run_id: Option<String>,
        session_id: Uuid,
        workspace: &Path,
        error: OrchError,
    ) -> OrchError {
        self.audit_err(
            &lease.tool,
            Some(&lease.request_id),
            Some(session_id),
            Some(&workspace.display().to_string()),
            &error,
        );
        lease.fail(run_id, error)
    }

    fn reaping_handles(&self) {
        let mut h = self.join_handles.lock();
        h.retain(|j| !j.is_finished());
    }

    /// Load run and verify workspace ownership against allowlist + session.
    fn load_authorized_run(&self, run_id: &str) -> Result<RunRecord, OrchError> {
        if safe_id_filename(run_id).is_err() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "malformed run_id",
            ));
        }
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        let allowlist = self.config.lock().allowlist.clone();
        let ws = PathBuf::from(&run.workspace);
        if !allowlist.contains(&ws) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "run workspace not authorized",
            ));
        }
        // Session must still match claimed workspace when present.
        if let Ok(session) = self.host.session_inspect(run.session_id) {
            if !session.cwd.is_empty() {
                let _ = require_workspace_match(&allowlist, Some(Path::new(&session.cwd)), &ws)
                    .map_err(|_| {
                        OrchError::new(
                            OrchErrorCode::ForbiddenScope,
                            "run session workspace mismatch",
                        )
                    })?;
            }
        }
        Ok(run)
    }

    // ── reads ──────────────────────────────────────────────────────────

    pub fn list_sessions(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let allowlist = self.config.lock().allowlist.clone();
        let sessions = self.host.list_sessions_by_kind(SessionKind::Build, false);
        let rows: Vec<serde_json::Value> = sessions
            .into_iter()
            .filter(|s| {
                if s.cwd.is_empty() {
                    return false;
                }
                allowlist.contains(Path::new(&s.cwd))
            })
            .map(|s| {
                let busy = self.host.session_busy(s.id);
                json!({
                    "sessionId": s.id,
                    "title": s.title,
                    "kind": "build",
                    "cwd": s.cwd,
                    "workspaceStatus": s.workspace_status.as_str(),
                    "updatedAt": s.updated_at,
                    "busy": busy,
                })
            })
            .collect();
        Ok(json!({ "sessions": rows }))
    }

    /// Create an allowlisted Build session for a remote coordinator.
    ///
    /// Session creation is intentionally narrower than the desktop API: the
    /// caller chooses only an existing configured workspace and an optional
    /// bounded title. All model, provider, and tool policy remains owned by
    /// the service host.
    pub fn create_session(
        &self,
        _auth: &AuthContext,
        workspace: &Path,
        title: Option<String>,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = canonical_workspace(workspace)?;
        if !self.config.lock().allowlist.contains(&claimed) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "workspace is not allowlisted by this service",
            ));
        }
        let summary = self
            .host
            .session_new_kind(SessionKind::Build)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let summary = self
            .host
            .session_set_cwd(summary.id, &claimed)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let summary = match title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(title) => self
                .host
                .session_rename(summary.id, title.to_string())
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            None => summary,
        };
        Ok(json!({
            "sessionId": summary.id,
            "title": summary.title,
            "workspace": summary.cwd,
            "updatedAt": summary.updated_at,
            "busy": false,
        }))
    }

    /// List durable agent identities whose workspaces are visible to this
    /// authenticated control-plane instance. Checkpoint contents remain a
    /// scoped read so listing cannot become a transcript or workspace oracle.
    pub fn list_persistent_agents(
        &self,
        auth: &AuthContext,
    ) -> Result<serde_json::Value, OrchError> {
        let allowlist = self.config.lock().allowlist.clone();
        let agents = self
            .host
            .list_persistent_agents()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .filter(|agent| {
                allowlist.contains(Path::new(&agent.workspace))
                    && agent
                        .owner_principal_id
                        .as_deref()
                        .is_none_or(|owner| owner == auth.owner_id)
            })
            .collect::<Vec<_>>();
        Ok(json!({ "agents": agents }))
    }

    /// List every durable Build run in one authorized session/workspace.
    ///
    /// Persistent-agent records intentionally point at the current run only;
    /// this read keeps completed and cancelled remote history reviewable
    /// without exposing runs from another session or workspace.
    pub fn list_runs_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_queue_request(session_id, workspace)?;
        let runs = self
            .store
            .list_runs()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .filter(|run| {
                run.session_id == session_id && run.workspace == claimed.display().to_string()
            })
            .collect::<Vec<_>>();
        Ok(json!({ "runs": runs }))
    }

    // ── durable workloads ----------------------------------------------

    fn authorize_work_read_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        let session = self
            .host
            .session_inspect(session_id)
            .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"))?;
        if session.kind != SessionKind::Build {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "only Build sessions can own durable work",
            ));
        }
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        require_workspace_match(&allowlist, cwd.as_deref(), workspace)
    }

    fn authorize_work_mutation_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        require_workspace_match(&allowlist, cwd.as_deref(), workspace)
    }

    fn load_work_scoped(
        &self,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        allow_archived: bool,
    ) -> Result<(WorkItem, PathBuf), OrchError> {
        let claimed = if allow_archived {
            self.authorize_work_read_scope(session_id, workspace)?
        } else {
            self.authorize_work_mutation_scope(session_id, workspace)?
        };
        let item = self
            .store
            .load_work_item(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown work_id"))?;
        if item.session_id != session_id || item.workspace != claimed.display().to_string() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "work item is outside the requested session scope",
            ));
        }
        Ok((item, claimed))
    }

    fn workload_value(
        &self,
        item: WorkItem,
        include_attempts: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let attempts = if include_attempts {
            self.store
                .list_work_attempts(Some(&item.work_id))
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .iter()
                .map(WorkAttemptView::from)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        Ok(json!({
            "work": item,
            "attempts": attempts,
        }))
    }

    async fn begin_work_mutation(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        payload: &serde_json::Value,
    ) -> Result<(PathBuf, IdempotencyStart), OrchError> {
        let claimed = match self.authorize_work_mutation_scope(session_id, workspace) {
            Ok(path) => path,
            Err(error) => {
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&workspace.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        let payload_hash = hash_payload(payload);
        let start = self
            .begin_idempotency(tool, request_id, &payload_hash, session_id, &claimed)
            .await?;
        Ok((claimed, start))
    }

    pub fn list_work_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let work = self
            .store
            .list_work_items()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .filter(|item| {
                item.session_id == session_id && item.workspace == claimed.display().to_string()
            })
            .collect::<Vec<_>>();
        Ok(json!({ "work": work }))
    }

    pub fn get_work_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let (item, _) = self.load_work_scoped(session_id, workspace, work_id, true)?;
        self.workload_value(item, true)
    }

    fn restore_claim_response(
        &self,
        mut response: serde_json::Value,
    ) -> Result<serde_json::Value, OrchError> {
        let attempt: WorkAttemptView =
            serde_json::from_value(response.get("attempt").cloned().ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "claim receipt omitted its durable attempt",
                )
            })?)
            .map_err(|_| {
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "claim receipt contains an invalid durable attempt",
                )
            })?;
        let actual = self
            .store
            .list_work_attempts(Some(&attempt.work_id))
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .find(|candidate| candidate.attempt_id == attempt.attempt_id)
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "claim receipt no longer has a durable attempt",
                )
            })?;
        let lease_secret = self.config.lock().bearer_token.clone();
        if lease_secret.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "claim lease cannot be recovered without the service credential",
            ));
        }
        let lease_token = actual.lease_token_for_secret(&lease_secret);
        if !actual.token_matches(&lease_token) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "claim lease cannot be recovered after credential rotation",
            ));
        }
        response["leaseToken"] = serde_json::Value::String(lease_token);
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        kind: String,
        objective: String,
        priority: i32,
        deadline: Option<chrono::DateTime<Utc>>,
        parent_work_id: Option<String>,
        dependencies: Vec<WorkDependency>,
        policy: WorkPolicy,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_create_work";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "kind": kind,
            "objective": objective,
            "priority": priority,
            "deadline": deadline,
            "parentWorkId": parent_work_id,
            "dependencies": dependencies,
            "policy": policy,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        let mut item = match WorkItem::new(
            kind,
            objective,
            session_id,
            claimed.display().to_string(),
            &auth.token_id,
            policy,
        ) {
            Ok(item) => item,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        item.priority = priority;
        item.deadline = deadline;
        item.parent_work_id = parent_work_id;
        item.dependencies = dependencies;
        if let Err(error) = item.validate() {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        if let Err(error) = self.store.save_work_item(&item) {
            return Err(self.fail_claim(
                &mut lease,
                Some(item.work_id.clone()),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, error.to_string()),
            ));
        }
        let response = self.workload_value(item.clone(), false)?;
        lease
            .complete(Some(item.work_id.clone()), response.clone())
            .map_err(|error| {
                self.fail_claim(&mut lease, Some(item.work_id), session_id, &claimed, error)
            })?;
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "durable work item created",
        );
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn claim_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        lease_ms: Option<u64>,
        agent_id: Option<String>,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_claim_work";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "leaseMs": lease_ms,
            "agentId": agent_id,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return self.restore_claim_response(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        let item = match self.load_work_scoped(session_id, &claimed, work_id, false) {
            Ok((item, _)) => item,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let lease_secret = self.config.lock().bearer_token.clone();
        if let Some(agent_id) = agent_id.as_deref() {
            if let Err(error) = self.store.require_agent_in_scope(
                agent_id,
                session_id,
                &claimed.display().to_string(),
            ) {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
            }
        }
        let claimant = agent_id.unwrap_or_else(|| auth.token_id.clone());
        let claim = match self.store.claim_work_with_lease_secret(
            work_id,
            &claimant,
            lease_ms,
            &lease_secret,
        ) {
            Ok(claim) => claim,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({
            "work": claim.work,
            "attempt": WorkAttemptView::from(&claim.attempt),
            "leaseToken": claim.lease_token,
            "sessionId": item.session_id,
            "workspace": claimed.display().to_string(),
        });
        let persisted_response = redact_claim_lease_token(response.clone());
        lease
            .complete(Some(work_id.to_string()), persisted_response)
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the authenticated MCP lease-renewal contract.
    pub async fn renew_work(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        lease_ms: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_lease_mutation(
            "ptah_renew_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({"attemptId": attempt_id, "leaseToken": lease_token, "leaseMs": lease_ms}),
            |store| store.renew_work_lease(work_id, attempt_id, lease_token, lease_ms),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Keeps the shared lease mutation boundary explicit.
    async fn work_lease_mutation<F>(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        details: serde_json::Value,
        operation: F,
    ) -> Result<serde_json::Value, OrchError>
    where
        F: FnOnce(&OrchStore) -> Result<WorkAttempt, OrchError>,
    {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "details": details,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let attempt = match operation(&self.store) {
            Ok(attempt) => attempt,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({
            "workId": work_id,
            "attempt": WorkAttemptView::from(&attempt),
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
        });
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the authenticated MCP run-link contract.
    pub async fn link_work_run(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        let response = self
            .work_lease_mutation(
                "ptah_link_work_run",
                request_id,
                session_id,
                workspace,
                work_id,
                json!({"attemptId": attempt_id, "leaseToken": lease_token, "runId": run_id}),
                |store| store.link_work_run(work_id, attempt_id, lease_token, &run.run_id),
            )
            .await?;
        let _ = auth;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the authenticated MCP progress-report contract.
    pub async fn report_work_progress(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        summary: String,
        percent: Option<u8>,
    ) -> Result<serde_json::Value, OrchError> {
        let progress = WorkProgress {
            summary,
            percent,
            updated_at: Utc::now(),
        };
        let payload_details =
            json!({"attemptId": attempt_id, "leaseToken": lease_token, "progress": progress});
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "details": payload_details,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                "ptah_report_work_progress",
                request_id,
                session_id,
                workspace,
                &payload,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let (item, attempt) =
            match self
                .store
                .report_work_progress(work_id, attempt_id, lease_token, progress)
            {
                Ok(value) => value,
                Err(error) => {
                    return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
                }
            };
        let response = json!({"work": item, "attempt": WorkAttemptView::from(&attempt)});
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the authenticated MCP lease-release contract.
    pub async fn release_work(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        reason: String,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_attempt_mutation(
            "ptah_release_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({"attemptId": attempt_id, "leaseToken": lease_token, "reason": reason}),
            |store| store.release_work(work_id, attempt_id, lease_token, &reason),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the authenticated MCP completion contract.
    pub async fn complete_work(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        result: WorkResult,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_attempt_mutation(
            "ptah_complete_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({"attemptId": attempt_id, "leaseToken": lease_token, "result": result}),
            |store| store.complete_work(work_id, attempt_id, lease_token, result),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the authenticated MCP failure contract.
    pub async fn fail_work(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        result: WorkResult,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_attempt_mutation(
            "ptah_fail_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({"attemptId": attempt_id, "leaseToken": lease_token, "result": result}),
            |store| store.fail_work(work_id, attempt_id, lease_token, result),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Keeps the shared attempt mutation boundary explicit.
    async fn work_item_attempt_mutation<F>(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        details: serde_json::Value,
        operation: F,
    ) -> Result<serde_json::Value, OrchError>
    where
        F: FnOnce(&OrchStore) -> Result<(WorkItem, super::workload::WorkAttempt), OrchError>,
    {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "details": details,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let (item, attempt) = match operation(&self.store) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({"work": item, "attempt": WorkAttemptView::from(&attempt)});
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)] // Keeps the authenticated cancellation contract revision-fenced.
    pub async fn cancel_work(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_cancel_work";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "reason": reason,
            "expectedRevision": expected_revision,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let (item, attempts) =
            match self
                .store
                .cancel_work_checked(work_id, &reason, expected_revision)
            {
                Ok(value) => value,
                Err(error) => {
                    return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
                }
            };
        let attempts = attempts
            .iter()
            .map(WorkAttemptView::from)
            .collect::<Vec<_>>();
        let response = json!({"work": item, "attempts": attempts});
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn assign_work(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        assigned_agent_id: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_mutation(
            "ptah_assign_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({
                "assignedAgentId": assigned_agent_id,
                "expectedRevision": expected_revision,
            }),
            move |store| store.assign_work(work_id, assigned_agent_id, expected_revision),
        )
        .await
    }

    fn authorize_worker_assignment(
        &self,
        workspace: &Path,
        session_id: Uuid,
        worker_agent_id: &str,
        work: &WorkItem,
        manager_agent_id: Option<&str>,
    ) -> Result<ScopedAssignment, OrchError> {
        let claimed = workspace.display().to_string();
        let worker = self
            .store
            .require_agent_in_scope(worker_agent_id, session_id, &claimed)?;
        if !super::workspaces_match(&worker.workspace, &work.workspace) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "cross-workspace assignment is not allowed",
            ));
        }
        let worker_spec = worker.current_spec()?.clone();
        let manager = manager_agent_id
            .map(|agent_id| {
                self.store
                    .require_agent_in_scope(agent_id, session_id, &claimed)
            })
            .transpose()?;
        let manager_spec = manager.as_ref().and_then(|agent| agent.spec.clone());
        let ceiling = self.config.lock().bounds.clone();
        reject_privilege_amplification(
            manager_spec.as_ref(),
            &worker_spec,
            &work.policy.bounds,
            &ceiling,
        )?;
        Ok(ScopedAssignment { worker, manager })
    }

    pub fn list_workers_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let workers = self
            .store
            .list_workers(&claimed.display().to_string(), Utc::now())?;
        Ok(json!({ "workers": workers }))
    }

    pub fn get_worker_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let worker = self
            .store
            .get_worker(agent_id, Utc::now())?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown worker"))?;
        if !super::workspaces_match(&worker.workspace, &claimed.display().to_string()) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "worker is outside the requested workspace",
            ));
        }
        Ok(json!({ "worker": worker }))
    }

    pub async fn heartbeat_worker(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
        host_kind: WorkerHostKind,
    ) -> Result<serde_json::Value, OrchError> {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "agentId": agent_id,
            "hostKind": host_kind,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                "ptah_heartbeat_worker",
                request_id,
                session_id,
                workspace,
                &payload,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        let presence = match self.store.heartbeat_worker_scoped(
            agent_id,
            &auth.token_id,
            host_kind,
            Utc::now(),
            session_id,
            &claimed.display().to_string(),
        ) {
            Ok(presence) => presence,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({ "presence": presence });
        lease
            .complete(Some(presence.agent_id.clone()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(presence.agent_id),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn offer_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        agent_id: &str,
        reason: String,
        expected_revision: Option<u64>,
        manager_agent_id: Option<String>,
    ) -> Result<serde_json::Value, OrchError> {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "agentId": agent_id,
            "reason": reason,
            "expectedRevision": expected_revision,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                "ptah_offer_work",
                request_id,
                session_id,
                workspace,
                &payload,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        let work = match self.load_work_scoped(session_id, &claimed, work_id, false) {
            Ok((work, _)) => work,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let actors = match self.authorize_worker_assignment(
            &claimed,
            session_id,
            agent_id,
            &work,
            manager_agent_id.as_deref(),
        ) {
            Ok(actors) => actors,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let manager_id = actors.manager.as_ref().map(|agent| agent.agent_id.clone());
        let (item, decision) = match self.store.offer_work(
            work_id,
            &actors.worker.agent_id,
            &auth.token_id,
            manager_id.as_deref(),
            &reason,
            expected_revision,
            Utc::now(),
        ) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({ "work": item, "decision": decision });
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn accept_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        agent_id: &str,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.worker_identity_mutation(
            "ptah_accept_work",
            auth,
            request_id,
            session_id,
            workspace,
            work_id,
            agent_id,
            reason,
            expected_revision,
            |store, agent_id, actor_id, reason, expected_revision, now| {
                store.accept_work(work_id, agent_id, actor_id, reason, expected_revision, now)
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn decline_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        agent_id: &str,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.worker_identity_mutation(
            "ptah_decline_work",
            auth,
            request_id,
            session_id,
            workspace,
            work_id,
            agent_id,
            reason,
            expected_revision,
            |store, agent_id, actor_id, reason, expected_revision, now| {
                store.decline_work(work_id, agent_id, actor_id, reason, expected_revision, now)
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn reassign_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        agent_id: &str,
        reason: String,
        expected_revision: Option<u64>,
        manager_agent_id: Option<String>,
    ) -> Result<serde_json::Value, OrchError> {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "agentId": agent_id,
            "reason": reason,
            "expectedRevision": expected_revision,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                "ptah_reassign_work",
                request_id,
                session_id,
                workspace,
                &payload,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        let work = match self.load_work_scoped(session_id, &claimed, work_id, false) {
            Ok((work, _)) => work,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let actors = match self.authorize_worker_assignment(
            &claimed,
            session_id,
            agent_id,
            &work,
            manager_agent_id.as_deref(),
        ) {
            Ok(actors) => actors,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let manager_id = actors.manager.as_ref().map(|agent| agent.agent_id.clone());
        let (item, decision) = match self.store.reassign_work(
            work_id,
            &actors.worker.agent_id,
            &auth.token_id,
            manager_id.as_deref(),
            &reason,
            expected_revision,
            Utc::now(),
        ) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({ "work": item, "decision": decision });
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn reprioritize_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        priority: i32,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_mutation(
            "ptah_reprioritize_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({"priority": priority, "reason": reason, "expectedRevision": expected_revision}),
            move |store| {
                store
                    .reprioritize_work(
                        work_id,
                        priority,
                        &auth.token_id,
                        &reason,
                        expected_revision,
                        Utc::now(),
                    )
                    .map(|(item, _)| item)
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn block_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_mutation(
            "ptah_block_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({"reason": reason, "expectedRevision": expected_revision}),
            move |store| {
                store
                    .block_work(
                        work_id,
                        &auth.token_id,
                        &reason,
                        expected_revision,
                        Utc::now(),
                    )
                    .map(|(item, _)| item)
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn request_work_review(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_mutation(
            "ptah_request_review",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({"reason": reason, "expectedRevision": expected_revision}),
            move |store| {
                store
                    .request_work_review(
                        work_id,
                        &auth.token_id,
                        &reason,
                        expected_revision,
                        Utc::now(),
                    )
                    .map(|(item, _)| item)
            },
        )
        .await
    }

    pub fn list_work_decisions_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let (item, _) = self.load_work_scoped(session_id, workspace, work_id, true)?;
        let decisions = self.store.list_work_decisions(&item.work_id)?;
        Ok(json!({ "workId": item.work_id, "decisions": decisions }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_message(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        kind: MessageKind,
        from_agent_id: Option<String>,
        to_agent_id: Option<String>,
        work_id: Option<String>,
        body: String,
        payload: Option<serde_json::Value>,
        reply_to_id: Option<String>,
        attempt_id: Option<String>,
        run_id: Option<String>,
    ) -> Result<serde_json::Value, OrchError> {
        let idempotency = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "kind": kind,
            "fromAgentId": from_agent_id,
            "toAgentId": to_agent_id,
            "workId": work_id,
            "body": body,
            "payload": payload,
            "replyToId": reply_to_id,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                "ptah_send_message",
                request_id,
                session_id,
                workspace,
                &idempotency,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Some(work_id) = &work_id {
            if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, true) {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
            }
        }
        let mut message = match WorkMessage::new(
            kind,
            auth.token_id.clone(),
            from_agent_id,
            to_agent_id,
            session_id,
            claimed.display().to_string(),
            work_id,
            body,
            payload,
            Utc::now(),
        ) {
            Ok(message) => message,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        message.reply_to_id = reply_to_id;
        message.attempt_id = attempt_id;
        message.run_id = run_id;
        if let Some(parent) = message.reply_to_id.as_deref() {
            if let Ok(Some(parent)) = self.store.load_message(parent) {
                if parent.expired_at(Utc::now()) && parent.kind == MessageKind::Question {
                    return Err(self.fail_claim(
                        &mut lease,
                        None,
                        session_id,
                        &claimed,
                        OrchError::new(OrchErrorCode::Conflict, "question has expired"),
                    ));
                }
                message.thread_id = parent.thread_id.or(Some(parent.message_id.clone()));
            }
        }
        let message = match self.store.send_message(message) {
            Ok(message) => message,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({ "message": message });
        lease
            .complete(Some(message.message_id.clone()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(message.message_id),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    pub async fn ack_message(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        message_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "messageId": message_id,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                "ptah_ack_message",
                request_id,
                session_id,
                workspace,
                &payload,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        let message = match self.store.ack_message_scoped(
            message_id,
            &auth.token_id,
            Utc::now(),
            session_id,
            &claimed.display().to_string(),
        ) {
            Ok(message) => message,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({ "message": message });
        lease
            .complete(Some(message_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(message_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    pub fn list_inbox_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
        after_seq: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let page = self.store.list_messages(
            session_id,
            &claimed.display().to_string(),
            after_seq,
            Some(agent_id),
            None,
            100,
        )?;
        Ok(json!(page))
    }

    pub fn list_outbox_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        actor_id: &str,
        after_seq: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let page = self.store.list_messages(
            session_id,
            &claimed.display().to_string(),
            after_seq,
            None,
            Some(actor_id),
            100,
        )?;
        Ok(json!(page))
    }

    pub fn message_activation_boundary(&self) -> OrchError {
        message_activation_unsupported()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn retry_work(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_mutation(
            "ptah_retry_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({
                "reason": reason,
                "expectedRevision": expected_revision,
            }),
            move |store| store.retry_work(work_id, &reason, expected_revision),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn approve_work(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        note: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        self.work_item_attempt_mutation(
            "ptah_approve_work",
            request_id,
            session_id,
            workspace,
            work_id,
            json!({
                "reviewerId": auth.token_id,
                "note": note,
                "expectedRevision": expected_revision,
            }),
            |store| store.approve_work(work_id, &auth.token_id, note, expected_revision),
        )
        .await
    }

    fn authorize_routine_read_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        self.authorize_work_read_scope(session_id, workspace)
    }

    fn authorize_routine_mutation_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        self.authorize_work_mutation_scope(session_id, workspace)
    }

    fn load_routine_scoped(
        &self,
        session_id: Uuid,
        workspace: &Path,
        routine_id: &str,
        allow_archived: bool,
    ) -> Result<(RoutineRecord, PathBuf), OrchError> {
        let claimed = if allow_archived {
            self.authorize_routine_read_scope(session_id, workspace)?
        } else {
            self.authorize_routine_mutation_scope(session_id, workspace)?
        };
        let routine = self
            .store
            .load_routine(routine_id)?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown routine_id"))?;
        if routine.session_id != session_id || routine.workspace != claimed.display().to_string() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "routine is outside the requested session scope",
            ));
        }
        Ok((routine, claimed))
    }

    fn routine_value(
        &self,
        routine: RoutineRecord,
        include_activations: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let activations = if include_activations {
            self.store.list_activations(&routine.routine_id, 32)?
        } else {
            Vec::new()
        };
        Ok(json!({
            "routine": routine,
            "activations": activations,
        }))
    }

    pub fn list_routines_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_routine_read_scope(session_id, workspace)?;
        let routines = self
            .store
            .list_routines()?
            .into_iter()
            .filter(|routine| {
                routine.session_id == session_id
                    && routine.workspace == claimed.display().to_string()
            })
            .collect::<Vec<_>>();
        Ok(json!({ "routines": routines }))
    }

    pub fn get_routine_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        routine_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let (routine, _) = self.load_routine_scoped(session_id, workspace, routine_id, true)?;
        self.routine_value(routine, true)
    }

    pub fn list_activations_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        routine_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let (routine, _) = self.load_routine_scoped(session_id, workspace, routine_id, true)?;
        let activations = self.store.list_activations(&routine.routine_id, 128)?;
        Ok(json!({
            "routineId": routine.routine_id,
            "activations": activations,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_routine(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        name: String,
        agent_id: String,
        trigger: RoutineTrigger,
        work_template: WorkTemplate,
        missed_run_policy: MissedRunPolicy,
        concurrency: RoutineConcurrencyPolicy,
        retry: RoutineRetryPolicy,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_create_routine";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "name": name,
            "agentId": agent_id,
            "trigger": trigger,
            "workTemplate": work_template,
            "missedRunPolicy": missed_run_policy,
            "concurrency": concurrency,
            "retry": retry,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let RoutineTrigger::External { adapter } = &trigger {
            return Err(self.fail_claim(
                &mut lease,
                None,
                session_id,
                &claimed,
                OrchError::new(
                    OrchErrorCode::Unsupported,
                    format!(
                        "{} adapters are reserved; they cannot create Work in this slice",
                        adapter.as_str()
                    ),
                ),
            ));
        }
        let agent = match self.store.load_agent(&agent_id) {
            Ok(Some(agent)) => agent,
            Ok(None) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    OrchError::new(OrchErrorCode::InvalidRequest, "unknown agent_id"),
                ));
            }
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    OrchError::new(OrchErrorCode::Internal, error.to_string()),
                ));
            }
        };
        if !super::workspaces_match(&agent.workspace, &claimed.display().to_string()) {
            return Err(self.fail_claim(
                &mut lease,
                None,
                session_id,
                &claimed,
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "agent source workspace does not match the requested workspace",
                ),
            ));
        }
        let now = Utc::now();
        let routine = match RoutineRecord::new(
            name,
            agent_id,
            session_id,
            claimed.display().to_string(),
            trigger,
            work_template,
            missed_run_policy,
            concurrency,
            retry,
            &auth.token_id,
            now,
        ) {
            Ok(routine) => routine,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        if let Err(error) = self.store.save_routine(&routine) {
            return Err(self.fail_claim(
                &mut lease,
                Some(routine.routine_id.clone()),
                session_id,
                &claimed,
                error,
            ));
        }
        let response = match self.routine_value(routine.clone(), false) {
            Ok(response) => response,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(routine.routine_id.clone()),
                    session_id,
                    &claimed,
                    error,
                ))
            }
        };
        lease
            .complete(Some(routine.routine_id.clone()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(routine.routine_id),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn set_routine_lifecycle(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        routine_id: &str,
        lifecycle: RoutineLifecycle,
        expected_revision: Option<u64>,
        tool: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "routineId": routine_id,
            "lifecycle": lifecycle,
            "expectedRevision": expected_revision,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_routine_scoped(session_id, &claimed, routine_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let routine = match self.store.set_routine_lifecycle(
            routine_id,
            lifecycle,
            expected_revision,
            Utc::now(),
        ) {
            Ok(routine) => routine,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = match self.routine_value(routine, false) {
            Ok(response) => response,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(routine_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                ))
            }
        };
        lease
            .complete(Some(routine_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(routine_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    pub async fn fire_routine(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        routine_id: &str,
        payload: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_fire_routine";
        let idempotency_payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "routineId": routine_id,
            "payload": payload,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                tool,
                request_id,
                session_id,
                workspace,
                &idempotency_payload,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        let routine = match self.load_routine_scoped(session_id, &claimed, routine_id, false) {
            Ok((routine, _)) => routine,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let now = Utc::now();
        let request = ActivationRequest {
            cause: ActivationCause::Manual,
            dedupe_key: manual_dedupe_key(routine_id, request_id),
            scheduled_at: now,
            received_at: now,
            payload,
            created_by: auth.token_id.clone(),
        };
        let ceiling = self.config.lock().bounds.clone();
        let activation = match self
            .store
            .activate_routine(routine_id, request, &ceiling, now)
        {
            Ok(activation) => activation,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({
            "activation": activation,
            "routineId": routine.routine_id,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
        });
        lease
            .complete(Some(routine_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(routine_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn work_item_mutation<F>(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        details: serde_json::Value,
        operation: F,
    ) -> Result<serde_json::Value, OrchError>
    where
        F: FnOnce(&OrchStore) -> Result<WorkItem, OrchError>,
    {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "details": details,
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let item = match operation(&self.store) {
            Ok(item) => item,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({
            "work": item,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
        });
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn worker_identity_mutation<F>(
        &self,
        tool: &str,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        agent_id: &str,
        reason: String,
        expected_revision: Option<u64>,
        operation: F,
    ) -> Result<serde_json::Value, OrchError>
    where
        F: FnOnce(
            &OrchStore,
            &str,
            &str,
            &str,
            Option<u64>,
            chrono::DateTime<Utc>,
        ) -> Result<(WorkItem, WorkDecision), OrchError>,
    {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "details": {
                "agentId": agent_id,
                "reason": reason,
                "expectedRevision": expected_revision,
            },
        });
        let (claimed, start) = self
            .begin_work_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        if let Err(error) =
            self.store
                .require_agent_in_scope(agent_id, session_id, &claimed.display().to_string())
        {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let (item, decision) = match operation(
            &self.store,
            agent_id,
            &auth.token_id,
            &reason,
            expected_revision,
            Utc::now(),
        ) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({
            "work": item,
            "decision": decision,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
        });
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    pub fn get_persistent_agent_scoped(
        &self,
        auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let _ =
            self.authorize_persistent_agent_request(auth, session_id, workspace, agent_id, false)?;
        let plan = self
            .host
            .prepare_agent_resume(session_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Conflict, error.to_string()))?;
        if plan.agent.agent_id != agent_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "persistent agent is not available in the requested scope",
            ));
        }
        serde_json::to_value(plan)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    /// Resume one verified persistent agent through the service adapter. The
    /// host owns the idempotency receipt and checkpoint validation; this layer
    /// adds workspace/session authorization and transport bounds.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume_persistent_agent(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
        prompt: String,
        max_rounds: Option<u32>,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_resume_persistent_agent";
        let (agent, claimed) = match self
            .authorize_persistent_agent_request(auth, session_id, workspace, agent_id, true)
        {
            Ok(value) => value,
            Err(error) => {
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&workspace.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        if let Err(error) = reject_control_prompt(&prompt) {
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&claimed.display().to_string()),
                &error,
            );
            return Err(error);
        }
        let bounds = self.config.lock().bounds.clone();
        if prompt.len() > bounds.max_prompt_bytes {
            let error = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "prompt exceeds max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                ),
            );
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&claimed.display().to_string()),
                &error,
            );
            return Err(error);
        }
        if let Some(rounds) = max_rounds {
            if rounds == 0 || rounds > bounds.max_rounds {
                let error = OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("max_rounds must be between 1 and {}", bounds.max_rounds),
                );
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&claimed.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        }
        let response = match self
            .host
            .resume_agent_with_request_id(session_id, prompt, max_rounds, Some(request_id.into()))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let error = OrchError::new(OrchErrorCode::Conflict, error.to_string());
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&claimed.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        let updated = self
            .host
            .get_persistent_agent(agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .unwrap_or(agent);
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "persistent agent resumed",
        );
        Ok(json!({
            "agent": updated,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "response": response,
        }))
    }

    pub fn set_managed_execution(
        &self,
        auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
        policy: ManagedExecutionPolicy,
    ) -> Result<serde_json::Value, OrchError> {
        policy.validate()?;
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let _ = self.store.require_agent_in_scope(
            agent_id,
            session_id,
            &claimed.display().to_string(),
        )?;
        if policy.enabled && (policy.bounds.max_total_tokens.is_none()) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "managed execution requires a finite token ceiling",
            ));
        }
        let agent = self
            .store
            .revise_agent_spec(agent_id, &auth.token_id, |spec| {
                if policy.enabled && spec.authority.computer_use_allowed {
                    anyhow::bail!("managed execution cannot grant Computer Use");
                }
                if policy.enabled {
                    spec.authority.bypass_permissions = false;
                }
                spec.managed_execution = policy.clone();
                Ok(())
            })
            .map_err(|error| OrchError::new(OrchErrorCode::InvalidRequest, error.to_string()))?
            .ok_or_else(|| {
                OrchError::new(OrchErrorCode::InvalidRequest, "unknown agent identity")
            })?;
        Ok(json!({
            "agent": agent,
            "managedExecution": agent.current_spec()?.managed_execution,
            "policyRevision": agent.current_spec()?.revision,
        }))
    }

    pub fn get_managed_execution(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let agent = self.store.require_agent_in_scope(
            agent_id,
            session_id,
            &claimed.display().to_string(),
        )?;
        Ok(json!({
            "agentId": agent.agent_id,
            "managedExecution": agent.current_spec()?.managed_execution,
            "policyRevision": agent.current_spec()?.revision,
            "executor": self.native_executor_status(),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn authorize_work_execution(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        work_id: &str,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "workId": work_id,
            "reason": reason,
            "expectedRevision": expected_revision,
        });
        let (claimed, start) = self
            .begin_work_mutation(
                "ptah_authorize_work_execution",
                request_id,
                session_id,
                workspace,
                &payload,
            )
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(response) => return Ok(response),
            IdempotencyStart::Perform(lease) => lease,
        };
        if let Err(error) = self.load_work_scoped(session_id, &claimed, work_id, false) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        let (item, decision) = match self.store.authorize_work_execution(
            work_id,
            &auth.token_id,
            None,
            &reason,
            expected_revision,
            Utc::now(),
        ) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error))
            }
        };
        let response = json!({ "work": item, "decision": decision });
        lease
            .complete(Some(work_id.to_string()), response.clone())
            .map_err(|error| {
                self.fail_claim(
                    &mut lease,
                    Some(work_id.to_string()),
                    session_id,
                    &claimed,
                    error,
                )
            })?;
        Ok(response)
    }

    pub fn list_execution_intents_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_read_scope(session_id, workspace)?;
        let intents = self
            .store
            .list_managed_intents()?
            .into_iter()
            .filter(|intent| {
                intent.session_id == session_id
                    && super::workspaces_match(&intent.workspace, &claimed.display().to_string())
            })
            .collect::<Vec<_>>();
        Ok(json!({ "intents": intents, "executor": self.native_executor_status() }))
    }

    pub fn resolve_work_input(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        permission_id: Uuid,
        allow: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_work_mutation_scope(session_id, workspace)?;
        let intent = self.store.resolve_parked_managed_permission(
            &permission_id.to_string(),
            session_id,
            &claimed.display().to_string(),
            Utc::now(),
        )?;
        let decision = if allow {
            crate::permission::PermissionDecision::Allow
        } else {
            crate::permission::PermissionDecision::Deny
        };
        let _ = self.host.permission_respond(permission_id, decision);
        Ok(json!({
            "permissionId": permission_id,
            "allow": allow,
            "sessionId": session_id,
            "workId": intent.work_id,
            "intentId": intent.intent_id,
        }))
    }

    pub fn get_capacity(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let max = self.host.orchestration_capacity_limit();
        let active = self.host.orchestration_active_count();
        let queued = self.host.orchestration_pending_count();
        let event_error = self
            .bus
            .last_persistence_error()
            .map(|error| self.bus.redact_text(&error, 500));
        let audit_error = self
            .store
            .last_audit_error()
            .map(|error| self.bus.redact_text(&error, 500));
        let run_error = self
            .store
            .last_run_error()
            .map(|error| self.bus.redact_text(&error, 500));
        let workload_supervisor = self
            .workload_supervisor
            .lock()
            .as_ref()
            .map(WorkloadSupervisor::status)
            .unwrap_or_else(|| {
                WorkloadSupervisorStatus::disabled(DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL)
            });
        let workload_supervisor_error = workload_supervisor
            .last_error
            .as_deref()
            .map(|error| self.bus.redact_text(error, 500));
        let routine_supervisor = self
            .routine_supervisor
            .lock()
            .as_ref()
            .map(RoutineSupervisor::status)
            .unwrap_or_else(|| RoutineSupervisorStatus::disabled(DEFAULT_ROUTINE_TICK_INTERVAL));
        let routine_supervisor_error = routine_supervisor
            .last_error
            .as_deref()
            .map(|error| self.bus.redact_text(error, 500));
        Ok(json!({
            "maxConcurrentRuns": max,
            "activeRuns": active,
            "available": max.saturating_sub(active),
            "queuedRuns": queued,
            "queueLimit": MAX_PENDING_ADMISSIONS,
            "health": {
                "laggedLiveEvents": self.bus.lagged_event_count(),
                "eventJournalPersistenceError": event_error,
                "auditPersistenceError": audit_error,
                "runPersistenceError": run_error,
                "workloadSupervisorError": workload_supervisor_error,
                "workloadSupervisor": workload_supervisor,
                "routineSupervisorError": routine_supervisor_error,
                "routineSupervisor": routine_supervisor,
                "nativeExecutorError": self.native_executor.lock().last_error.clone().map(|error| self.bus.redact_text(&error, 500)),
                "nativeExecutor": self.native_executor.lock().clone(),
            },
        }))
    }

    pub fn get_run(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.run_value(self.load_authorized_run(run_id)?)
    }

    pub fn get_run_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.run_value(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn run_value(&self, mut run: RunRecord) -> Result<serde_json::Value, OrchError> {
        self.refresh_queue_position(&mut run);
        serde_json::to_value(run)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    pub fn get_progress(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.progress_value(self.load_authorized_run(run_id)?)
    }

    pub fn get_progress_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.progress_value(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn progress_value(&self, mut run: RunRecord) -> Result<serde_json::Value, OrchError> {
        self.refresh_queue_position(&mut run);
        let busy = self.host.session_busy(run.session_id);
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "queuePosition": run.queue_position,
            "busy": busy,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
            "promptPreview": run.prompt_preview,
            "progress": run.progress,
            "createdAt": run.created_at,
            "updatedAt": run.updated_at,
            "terminalResult": run.terminal_result,
            "stopCause": run.stop_cause,
            "bounds": run.bounds,
            "errorCode": run.error_code,
        }))
    }

    fn refresh_queue_position(&self, run: &mut RunRecord) {
        run.queue_position = if run.state == RunState::Queued {
            self.host.orchestration_pending_position(&run.run_id)
        } else {
            None
        };
    }

    pub fn get_events(
        &self,
        _auth: &AuthContext,
        run_id: Option<&str>,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        // run_id is required — never fall back to the global journal.
        let rid = run_id.ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "run_id is required for get_events",
            )
        })?;
        let run = self.load_authorized_run(rid)?;
        self.events_for_run(run, after_seq, limit)
    }

    pub fn get_events_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        self.events_for_run(
            self.authorize_run_request(session_id, workspace, run_id)?,
            after_seq,
            limit,
        )
    }

    /// Authorize a run and return its current journal bounds plus an initial
    /// durable page for the optional Streamable HTTP live channel.
    pub(crate) fn live_run_page(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<(LiveRunScope, JournalPage), OrchError> {
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        let Some(start_seq) = run.start_seq else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "run has not started; use ptah_get_progress and open the live stream once running",
            ));
        };
        let scope = LiveRunScope {
            session_id: run.session_id,
            run_id: run.run_id.clone(),
            start_seq,
            end_seq: run.end_seq,
        };
        let page = self.events_page_for_run(run, after_seq, limit)?;
        Ok((scope, page))
    }

    pub(crate) fn subscribe_events(&self) -> EventReceiver {
        self.bus.subscribe()
    }

    // ── Computer Run reads (#271 slice 2) ──────────────────────────────
    //
    // Read-only projections of the durable Computer Run ledger. Mutations
    // deliberately remain absent from the control plane.

    /// Backend-free scoped reader over the host's shared Computer Run store.
    /// Availability is global and session-independent, so this failure leaks
    /// nothing about any run or session.
    fn computer_reads(&self) -> Result<crate::computer_use::ComputerRunReads, OrchError> {
        self.host
            .ensure_computer_store()
            .map(crate::computer_use::ComputerRunReads::new)
            .map_err(|_| {
                OrchError::new(
                    OrchErrorCode::Unsupported,
                    "computer use is unavailable on this host",
                )
            })
    }

    /// Session + workspace gate shared by every Computer Run read. Computer
    /// Runs are owned by build and chat sessions alike, so this requires the
    /// session to exist and match the claimed allowlisted workspace — not to
    /// be a Build session. Archived Lanes remain readable through this path;
    /// archive is an execution boundary, not deletion of durable evidence.
    ///
    /// The claimed workspace is allowlisted first (session-independent).
    /// Unknown session, missing cwd, and cwd mismatch then collapse into the
    /// same `forbidden_scope` as an unauthorized run, so session existence
    /// is not distinguishable from cross-scope.
    fn authorize_computer_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<String, OrchError> {
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = super::authz::canonical_workspace(workspace)?;
        if !allowlist.contains(&claimed) {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "workspace not in allowlist",
            ));
        }
        // Authorization must never promote the requested Lane into the local
        // operator cockpit. In particular, do not use `session_load`: it
        // changes the active Lane, project, tab strip, MCP servers, skills,
        // and persisted desktop chrome as part of opening a Lane for work.
        let session = self.host.session_inspect(session_id).ok();
        let cwd = session
            .as_ref()
            .and_then(|loaded| (!loaded.cwd.is_empty()).then(|| PathBuf::from(&loaded.cwd)));
        let Some(cwd) = cwd else {
            return Err(computer_scope_denied());
        };
        let session_cwd =
            super::authz::canonical_workspace(&cwd).map_err(|_| computer_scope_denied())?;
        if session_cwd != claimed {
            return Err(computer_scope_denied());
        }
        Ok(claimed.display().to_string())
    }

    pub fn list_computer_runs_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let runs = reads
            .list_run_projections(binding, Utc::now())
            .map_err(computer_read_error)?;
        Ok(json!({ "runs": runs }))
    }

    pub fn get_computer_run_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let projection = reads
            .project_run(binding, run_id, Utc::now())
            .map_err(computer_read_error)?;
        serde_json::to_value(projection)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    pub fn get_computer_run_events_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let page = reads
            .run_events(binding, run_id, after_seq, limit)
            .map_err(computer_read_error)?;
        if page.cursor_expired {
            // Same 410 idiom as `ptah_get_events`, but the retained window
            // rides the error so recovery does not require a second get.
            return Err(OrchError::with_data(
                OrchErrorCode::CursorExpired,
                "computer run event cursor is below the retained window; resume from eventRange",
                json!({
                    "eventRange": page.range.map(|range| json!({
                        "startSeq": range.start_seq,
                        "endSeq": range.end_seq,
                    })),
                }),
            ));
        }
        serde_json::to_value(page)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    pub fn get_computer_capacity_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let capacity = reads.capacity(binding).map_err(computer_read_error)?;
        serde_json::to_value(capacity)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn events_for_run(
        &self,
        run: RunRecord,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        serde_json::to_value(self.events_page_for_run(run, after_seq, limit)?)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    fn events_page_for_run(
        &self,
        run: RunRecord,
        after_seq: u64,
        limit: usize,
    ) -> Result<JournalPage, OrchError> {
        // Read the bounded run range before applying the caller's page limit.
        // Applying `limit` to the global journal first can return a page made
        // entirely of other sessions and advance the cursor past this run's
        // events. `read_range_all` is bounded by the journal retention policy
        // and preserves cursor-expiry failures instead of silently skipping.
        let mut entries = self
            .bus
            .read_range_all(after_seq, run.end_seq, Some(run.session_id))
            .map_err(|_| {
                OrchError::new(
                    OrchErrorCode::CursorExpired,
                    "event cursor expired; restart from seq 0 or latest",
                )
            })?;
        entries.retain(|e| {
            run.start_seq.map(|s| e.seq >= s).unwrap_or(true)
                && run.end_seq.map(|s| e.seq <= s).unwrap_or(true)
        });
        entries.truncate(limit.clamp(1, 500));
        let next_cursor = entries.last().map(|e| e.seq);
        Ok(JournalPage {
            entries,
            next_cursor,
            cursor_expired: false,
        })
    }

    pub fn get_changes(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.changes_for_run(self.load_authorized_run(run_id)?)
    }

    pub fn get_changes_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.changes_for_run(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn changes_for_run(&self, run: RunRecord) -> Result<serde_json::Value, OrchError> {
        // Prefer durable aggregates (survive journal rollover).
        let mut paths: Vec<serde_json::Value> = run
            .aggregates
            .changes
            .iter()
            .map(|c| json!({ "path": c.path, "summary": c.summary }))
            .collect();
        if let Ok(entries) = self.scoped_events_complete(&run) {
            for e in entries {
                if let crate::events::SessionUpdate::FileEdit { path, summary, .. } = e.update {
                    if !paths.iter().any(|p| p["path"] == path) {
                        paths.push(json!({ "path": path, "summary": summary }));
                    }
                }
            }
        }
        Ok(json!({ "runId": run.run_id, "changes": paths }))
    }

    pub fn get_test_results(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.test_results_for_run(self.load_authorized_run(run_id)?)
    }

    pub fn get_test_results_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.test_results_for_run(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn test_results_for_run(&self, run: RunRecord) -> Result<serde_json::Value, OrchError> {
        let mut by_id: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        // Seed from durable aggregates.
        for t in &run.aggregates.tests {
            by_id.insert(
                t.call_id.clone(),
                json!({
                    "callId": t.call_id,
                    "command": t.command,
                    "status": t.status,
                    "exitCode": t.exit_code,
                    "cancelled": t.cancelled,
                }),
            );
        }
        if let Ok(entries) = self.scoped_events_complete(&run) {
            for e in entries {
                match e.update {
                    crate::events::SessionUpdate::ShellSessionStarted {
                        command, call_id, ..
                    } => {
                        if is_recognized_test_command(&command) {
                            by_id.insert(
                                call_id.clone(),
                                json!({
                                    "callId": call_id,
                                    "command": command,
                                    "status": "started",
                                }),
                            );
                        }
                    }
                    crate::events::SessionUpdate::ShellSessionEnded {
                        call_id,
                        exit_code,
                        cancelled,
                        ..
                    } => {
                        if let Some(prev) = by_id.get_mut(&call_id) {
                            prev["status"] = json!("ended");
                            prev["exitCode"] = json!(exit_code);
                            prev["cancelled"] = json!(cancelled);
                        }
                        // Do NOT record non-test shell ends.
                    }
                    _ => {}
                }
            }
        }
        let observed: Vec<_> = by_id.into_values().collect();
        if observed.is_empty() {
            Ok(json!({
                "runId": run.run_id,
                "status": "not_observed",
                "results": [],
            }))
        } else {
            Ok(json!({
                "runId": run.run_id,
                "status": "observed",
                "results": observed,
            }))
        }
    }

    pub fn get_handoff(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.handoff_for_run(self.load_authorized_run(run_id)?)
    }

    pub fn get_handoff_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.handoff_for_run(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn handoff_for_run(&self, run: RunRecord) -> Result<serde_json::Value, OrchError> {
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "finalResponse": run.final_response,
            "terminalResult": run.terminal_result,
            "stopCause": run.stop_cause,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
            "bounds": run.bounds,
            "changes": run.aggregates.changes,
            "tests": run.aggregates.tests,
            "verification": run.aggregates.verification,
            "usage": run.aggregates.usage,
            "usageComplete": run.aggregates.usage_complete,
            "usagePendingRequests": run.aggregates.usage_pending_requests,
        }))
    }

    fn scoped_events_complete(
        &self,
        run: &RunRecord,
    ) -> Result<Vec<crate::event_bus::JournalEntry>, OrchError> {
        let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
        match self
            .bus
            .read_range_all(after, run.end_seq, Some(run.session_id))
        {
            Ok(v) => Ok(v),
            Err(CursorExpiredError) => Err(OrchError::new(
                OrchErrorCode::CursorExpired,
                "event cursor expired for run range",
            )),
        }
    }

    fn require_build_session(
        &self,
        session_id: Uuid,
    ) -> Result<crate::session::SessionSummary, OrchError> {
        let session = self
            .host
            .session_inspect(session_id)
            .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"))?;
        if session.archived {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "archived Lane is inspection-only; restore it before controlling it",
            ));
        }
        if session.kind != SessionKind::Build {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "only Build sessions are controllable in this slice",
            ));
        }
        if session.workspace_status != WorkspaceStatus::Ready {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                format!(
                    "session workspace is {}: choose a working directory before controlling it",
                    session.workspace_status.as_str()
                ),
            ));
        }
        Ok(session)
    }

    fn authorize_run_request(
        &self,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<RunRecord, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        if run.session_id != session_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "run does not belong to the requested session",
            ));
        }
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        if claimed.display().to_string() != run.workspace {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "run workspace does not match the requested workspace",
            ));
        }
        Ok(run)
    }

    fn authorize_persistent_agent_request(
        &self,
        auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
        claim_owner: bool,
    ) -> Result<(AgentRecord, PathBuf), OrchError> {
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        let agent = self
            .host
            .get_persistent_agent(agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "persistent agent is not available in the requested scope",
                )
            })?;
        let agent_workspace = canonical_workspace(Path::new(&agent.workspace))?;
        if !agent.known_lane_ids().contains(&session_id) || agent_workspace != claimed {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "persistent agent is not available in the requested scope",
            ));
        }
        if agent
            .owner_principal_id
            .as_deref()
            .is_some_and(|owner| owner != auth.owner_id)
        {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "persistent agent is not owned by this service account",
            ));
        }
        let agent = if claim_owner {
            self.store
                .claim_agent_owner(&agent.agent_id, &auth.owner_id)
                .map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::ForbiddenScope,
                        format!("persistent agent is not owned by this service account: {error}"),
                    )
                })?
                .ok_or_else(|| {
                    OrchError::new(
                        OrchErrorCode::ForbiddenScope,
                        "persistent agent is not available in the requested scope",
                    )
                })?
        } else {
            agent
        };
        Ok((agent, claimed))
    }

    fn authorize_queue_request(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        require_workspace_match(&allowlist, cwd.as_deref(), workspace)
    }

    async fn begin_queue_mutation(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        payload: &serde_json::Value,
    ) -> Result<(PathBuf, IdempotencyStart), OrchError> {
        let claimed = match self.authorize_queue_request(session_id, workspace) {
            Ok(path) => path,
            Err(error) => {
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&workspace.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        let payload_hash = hash_payload(payload);
        let start = match self
            .begin_idempotency(tool, request_id, &payload_hash, session_id, &claimed)
            .await
        {
            Ok(start) => start,
            Err(error) => {
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&claimed.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        Ok((claimed, start))
    }

    fn queue_error(error: anyhow::Error) -> OrchError {
        let message = error.to_string();
        let code = if message.contains("stale queued prompt version")
            || message.contains("stale prompt queue revision")
        {
            OrchErrorCode::StaleVersion
        } else if message.contains("unknown queued prompt")
            || message.contains("no prompt queue for session")
        {
            OrchErrorCode::InvalidRequest
        } else {
            OrchErrorCode::Internal
        };
        OrchError::new(code, message)
    }

    #[allow(clippy::too_many_arguments)]
    fn queue_response(
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        action: &str,
        entries: Vec<PromptQueueEntry>,
        changed_entry: Option<PromptQueueEntry>,
        disposition: Option<SteeringDisposition>,
        revision: u64,
    ) -> serde_json::Value {
        json!({
            "requestId": request_id,
            "actionId": request_id,
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "origin": "mcp",
            "action": action,
            "disposition": disposition,
            "actionVersion": changed_entry.as_ref().map(|entry| entry.version),
            // The queue revision this mutation produced. Reorder is fenced on
            // it, so a coordinator that had to re-read the queue after every
            // other verb could never chain a mutation into a reorder without a
            // window for someone else to move first. Every receipt now carries
            // the revision its own mutation stamped.
            "revision": revision,
            "entry": changed_entry,
            "entries": entries,
        })
    }

    pub fn get_queue(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_queue_request(session_id, workspace)?;
        let snapshot = self
            .host
            .session_queue_snapshot(session_id)
            .map_err(Self::queue_error)?;
        Ok(json!({
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "revision": snapshot.revision,
            "entries": snapshot.entries,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn edit_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        version: u64,
        text: String,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_edit_queue";
        if let Err(error) = reject_control_prompt(&text) {
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            return Err(error);
        }
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "version": version,
            "text": text,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, revision) = match self
            .host
            .session_queue_edit_with_origin(session_id, entry_id, version, text, "mcp")
        {
            Ok(entries) => entries,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let changed_entry = entries.iter().find(|entry| entry.id == entry_id).cloned();
        let response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "edited",
            entries,
            changed_entry,
            None,
            revision,
        );
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue edited",
        );
        Ok(response)
    }

    pub async fn remove_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_remove_queue";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "expectedVersion": expected_version,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, changed_entry, revision) = match self
            .host
            .session_queue_remove_with_origin_receipt(session_id, entry_id, "mcp", expected_version)
        {
            Ok(entries) => entries,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "removed",
            entries,
            Some(changed_entry),
            None,
            revision,
        );
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue entry removed",
        );
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn reorder_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        to_index: usize,
        expected_version: u64,
        expected_revision: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_reorder_queue";
        self.reject_selecting_control_entry(tool, request_id, session_id, workspace, entry_id)?;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "toIndex": to_index,
            "expectedVersion": expected_version,
            "expectedRevision": expected_revision,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, revision) = match self.host.session_queue_move_with_origin_and_revision(
            session_id,
            entry_id,
            to_index,
            "mcp",
            expected_version,
            expected_revision,
        ) {
            Ok(result) => result,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let changed_entry = entries.iter().find(|entry| entry.id == entry_id).cloned();
        let mut response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "reordered",
            entries,
            changed_entry,
            None,
            revision,
        );
        response["revision"] = json!(revision);
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue reordered",
        );
        Ok(response)
    }

    pub async fn clear_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_clear_queue";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, outcome, revision) = match self
            .host
            .session_queue_clear_with_origin_receipt(session_id, "mcp")
        {
            Ok(result) => result,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let mut response = Self::queue_response(
            request_id, session_id, &claimed, "cleared", entries, None, None, revision,
        );
        // An empty `entries` list alone would be a fail-open receipt: steering
        // already handed to a model boundary cannot be retracted and will
        // still be injected. Report it rather than implying the session is
        // quiet. `stopped` is the field a coordinator should branch on.
        if let Some(object) = response.as_object_mut() {
            object.insert("clearedQueued".into(), json!(outcome.queued_cleared));
            object.insert(
                "steeringCancelled".into(),
                json!(outcome.steering_cancelled),
            );
            object.insert("steeringInFlight".into(), json!(outcome.steering_in_flight));
            object.insert("stopped".into(), json!(outcome.fully_stopped()));
        }
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue cleared",
        );
        Ok(response)
    }

    /// S7: the control plane must not be able to *schedule* a prompt it is
    /// forbidden from *creating*.
    ///
    /// `reject_control_prompt` blocks `!` and `/` prompts on every path that
    /// authors text, but selection verbs took an entry id and never looked at
    /// what they were selecting. A locally authored `/yolo` or `!rm ...` could
    /// therefore be promoted to the head of the queue, and `run_next` would
    /// cancel the active turn to make it run — the forbidden outcome reached
    /// by choosing instead of by writing. Selection is now held to the same
    /// policy as authorship, evaluated against the stored text.
    ///
    /// Reading the entry before claiming the mutation is safe against edits in
    /// the gap: changing the text bumps the entry version, so the caller's
    /// `expected_version` fails closed.
    fn reject_selecting_control_entry(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
    ) -> Result<(), OrchError> {
        // Authorize before reading. This runs ahead of `begin_queue_mutation`,
        // which does its own authorization, so without this an unscoped caller
        // could learn something about another workspace's queue from whether
        // the policy rejected it.
        self.authorize_queue_request(session_id, workspace)?;
        let entries = self.host.session_queue_list(session_id).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("queue unavailable: {error}"),
            )
        })?;
        let Some(entry) = entries.into_iter().find(|entry| entry.id == entry_id) else {
            // Leave "unknown entry" to the mutator, so the not-found contract
            // stays in one place.
            return Ok(());
        };
        if let Err(error) = reject_control_prompt(&entry.text) {
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            return Err(error);
        }
        Ok(())
    }

    pub async fn run_next_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_run_next";
        self.reject_selecting_control_entry(tool, request_id, session_id, workspace, entry_id)?;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "expectedVersion": expected_version,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (result, revision) = match self.host.session_queue_run_next_with_origin(
            session_id,
            entry_id,
            "mcp",
            expected_version,
        ) {
            Ok(result) => result,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        // `run_next` removes the entry from the durable queue, so the host
        // returns the changed entry separately from the post-action snapshot.
        let changed_entry = Some(result.changed_entry.clone());
        let mut response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "run_next",
            result.entries,
            changed_entry,
            None,
            revision,
        );
        response["cancelledActive"] = json!(result.cancelled_active);
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue entry promoted to run next",
        );
        Ok(response)
    }

    pub async fn steer_queued(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_steer_queued";
        self.reject_selecting_control_entry(tool, request_id, session_id, workspace, entry_id)?;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "expectedVersion": expected_version,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (receipt, revision) = match self.host.session_queue_steer_entry_with_origin(
            session_id,
            entry_id,
            "mcp",
            expected_version,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "steer_now",
            receipt.entries,
            Some(receipt.entry),
            Some(receipt.disposition),
            revision,
        );
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queued entry steered without cancelling",
        );
        Ok(response)
    }

    fn isolated_review(
        &self,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<(RunRecord, crate::run_promotion::RunReview), OrchError> {
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        if run.state != RunState::Completed {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "only completed runs can be reviewed",
            ));
        }
        let Some(execution) = run.execution.as_ref() else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "run used shared execution and has no isolated diff",
            ));
        };
        if execution.mode != RunExecutionMode::IsolatedWorktree
            || execution.promotion_state != PromotionState::Ready
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated run is not ready for review",
            ));
        }
        let review = self
            .host
            .inspect_run(session_id, run_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Conflict, error.to_string()))?;
        if execution.final_fingerprint.as_deref() != Some(review.fingerprint.as_str()) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated worktree fingerprint changed; review is stale",
            ));
        }
        Ok((run, review))
    }

    pub fn review_run(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let (run, review) = self.isolated_review(session_id, workspace, run_id)?;
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "sourceFingerprint": run.execution.as_ref().map(|e| e.source_fingerprint.clone()),
            "finalFingerprint": review.fingerprint,
            "changedFiles": review.changed_files,
            "diff": review.diff,
            "diffTruncated": review.diff_truncated,
            "promotionState": run.execution.as_ref().map(|e| e.promotion_state),
        }))
    }

    #[allow(clippy::too_many_arguments)] // Keeps the approval scope explicit at the control boundary.
    pub async fn approve_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        source_fingerprint: String,
        final_fingerprint: String,
        changed_files: Vec<ChangeRecord>,
        ttl_ms: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        const DEFAULT_TTL_MS: u64 = 5 * 60 * 1_000;
        const MAX_TTL_MS: u64 = 15 * 60 * 1_000;
        let tool = "ptah_approve_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
            "sourceFingerprint": source_fingerprint,
            "finalFingerprint": final_fingerprint,
            "changedFiles": changed_files,
            "ttlMs": ttl_ms,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, error: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            error
        };
        let ttl = ttl_ms.unwrap_or(DEFAULT_TTL_MS);
        if ttl == 0 || ttl > MAX_TTL_MS {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "ttl_ms must be between 1 and 900000",
                ),
            ));
        }
        let run = match self.authorize_run_request(session_id, workspace, run_id) {
            Ok(run) => run,
            Err(error) => return Err(fail(self, error)),
        };
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await
        {
            Ok(IdempotencyStart::Replay(value)) => return Ok(value),
            Ok(IdempotencyStart::Perform(lease)) => lease,
            Err(error) => return Err(error),
        };
        let (run, review) = match self.isolated_review(session_id, workspace, run_id) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    error,
                ))
            }
        };
        let Some(execution) = run.execution.as_ref() else {
            unreachable!("isolated_review guarantees execution");
        };
        if source_fingerprint != execution.source_fingerprint
            || final_fingerprint != review.fingerprint
            || changed_files != review.changed_files
        {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "approval scope does not match the current reviewed diff",
                ),
            ));
        }
        if let Some(existing) = run.approval.as_ref() {
            if existing.expires_at > Utc::now() {
                let error = OrchError::new(
                    OrchErrorCode::Conflict,
                    "an unexpired approval already exists for this run",
                );
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    error,
                ));
            }
        }
        let issued_at = Utc::now();
        let approval = RunApproval {
            approval_id: Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            session_id,
            workspace: run.workspace.clone(),
            source_fingerprint,
            final_fingerprint,
            changed_files,
            issued_at,
            expires_at: issued_at + chrono::Duration::milliseconds(ttl as i64),
        };
        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "approvalId": approval.approval_id,
            "expiresAt": approval.expires_at,
            "sourceFingerprint": approval.source_fingerprint,
            "finalFingerprint": approval.final_fingerprint,
            "changedFiles": approval.changed_files,
        });
        let updated = self.store.update_run(run_id, |current| {
            current.approval = Some(approval.clone());
            current.updated_at = Utc::now();
            Ok(())
        });
        let updated = match updated {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(anyhow::anyhow!("run disappeared while approving")),
            Err(error) => Err(error),
        };
        if let Err(error) = updated {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                OrchError::new(OrchErrorCode::Internal, error.to_string()),
            ));
        }
        if let Err(error) = lease.complete(Some(run_id.to_string()), response.clone()) {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                error,
            ));
        }
        Ok(response)
    }

    pub async fn promote_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        approval_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_promote_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
            "approvalId": approval_id,
        });
        let phash = hash_payload(&payload);
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let promoted =
            match self
                .host
                .promote_run_with_approval(session_id, run_id, Some(approval_id))
            {
                Ok(run) => run,
                Err(error) => {
                    return Err(self.fail_claim(
                        &mut lease,
                        Some(run_id.to_string()),
                        session_id,
                        Path::new(&run.workspace),
                        OrchError::new(OrchErrorCode::Conflict, error.to_string()),
                    ))
                }
            };
        let response = serde_json::to_value(promoted)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        lease.complete(Some(run_id.to_string()), response.clone())?;
        Ok(response)
    }

    pub async fn discard_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_discard_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
        });
        let phash = hash_payload(&payload);
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        if !run.state.is_terminal() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "only terminal runs can be discarded",
            ));
        }
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let discarded = match self.host.discard_run(session_id, run_id) {
            Ok(run) => run,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    OrchError::new(OrchErrorCode::Conflict, error.to_string()),
                ))
            }
        };
        let response = serde_json::to_value(discarded)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        lease.complete(Some(run_id.to_string()), response.clone())?;
        Ok(response)
    }

    // ── mutations ──────────────────────────────────────────────────────

    pub async fn submit_task(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            RunExecutionMode::Shared,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Keeps bounded submission policy explicit at the control boundary.
    pub async fn submit_task_with_execution_mode(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode_and_queue(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            execution_mode,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn submit_task_with_execution_mode_and_queue(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
        allow_queue: bool,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode_and_queue_parent(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            execution_mode,
            allow_queue,
            None,
            "ptah_submit_task",
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_task_with_execution_mode_and_queue_parent(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
        allow_queue: bool,
        retry_of: Option<&str>,
        idempotency_tool: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = idempotency_tool;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "bounds": bounds_json,
            "executionMode": execution_mode,
            "allowQueue": allow_queue,
            "retryOf": retry_of,
        });
        let phash = hash_payload(&payload);

        let finish_err = |svc: &Self, e: OrchError| -> OrchError {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };

        if let Err(e) = reject_control_prompt(&prompt) {
            return Err(finish_err(self, e));
        }
        let ceiling = self.config.lock().bounds.clone();
        let mut bounds = match merge_bounds(&ceiling, bounds_json.as_ref()) {
            Ok(b) => b,
            Err(e) => return Err(finish_err(self, e)),
        };
        if prompt.len() > bounds.max_prompt_bytes {
            let e = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "prompt exceeds max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                ),
            );
            return Err(finish_err(self, e));
        }

        let session = match self.require_build_session(session_id) {
            Ok(s) => s,
            Err(e) => return Err(finish_err(self, e)),
        };
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(finish_err(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let agent = match self.host.ensure_session_agent(session_id) {
            Ok(agent) => agent,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    OrchError::new(OrchErrorCode::Internal, error.to_string()),
                ));
            }
        };
        let agent = match self
            .store
            .claim_agent_owner(&agent.agent_id, &auth.owner_id)
        {
            Ok(Some(agent)) => agent,
            Ok(None) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    OrchError::new(
                        OrchErrorCode::ForbiddenScope,
                        "persistent Agent is not available",
                    ),
                ));
            }
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    OrchError::new(
                        OrchErrorCode::ForbiddenScope,
                        format!("persistent Agent is owned by another service account: {error}"),
                    ),
                ));
            }
        };
        let agent_bounds = match agent.current_spec() {
            Ok(spec) => &spec.default_run_bounds,
            Err(error) => {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
            }
        };
        bounds.max_prompt_bytes = bounds.max_prompt_bytes.min(agent_bounds.max_prompt_bytes);
        bounds.max_rounds = bounds.max_rounds.min(agent_bounds.max_rounds);
        bounds.max_duration_ms = bounds.max_duration_ms.min(agent_bounds.max_duration_ms);
        bounds.max_total_tokens = match (bounds.max_total_tokens, agent_bounds.max_total_tokens) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        };
        if prompt.len() > bounds.max_prompt_bytes {
            let error = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "prompt exceeds persistent Agent max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                ),
            );
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }

        // Give older queued work first claim on any newly available capacity.
        self.pump_pending();
        let run_id = Uuid::new_v4().to_string();
        let queue_ahead = self.host.orchestration_pending_count() > 0;
        let mut queued = false;
        if allow_queue && queue_ahead {
            queued = true;
        } else if let Err(e) = self.try_reserve_capacity(&run_id, session_id) {
            if allow_queue
                && matches!(
                    e.code,
                    OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted
                )
            {
                queued = true;
            } else {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
            }
        }
        let start_seq = (!queued).then(|| self.bus.next_seq());
        let agent_spec_revision = agent
            .current_spec()
            .map_err(|error| self.fail_claim(&mut lease, None, session_id, &claimed, error))?
            .revision;
        let run = RunRecord {
            run_id: run_id.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            request_id: request_id.into(),
            // Distinguish coordinator-owned work from desktop turns so the
            // desktop can surface external activity without guessing from
            // transport timing.
            client_id: Some(if auth.token_id == "primary" {
                // Preserve the established wire value for the compatibility
                // credential; newly named device credentials are emitted by
                // their stable IDs.
                "mcp".into()
            } else {
                auth.token_id.clone()
            }),
            state: if queued {
                RunState::Queued
            } else {
                RunState::Running
            },
            agent_id: Some(agent.agent_id),
            retry_of: retry_of.map(str::to_string),
            parent_run_id: None,
            agent_spec_revision: Some(agent_spec_revision),
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: bounds.clone(),
            prompt_preview: self.bus.redact_text(&prompt_preview(&prompt), 500),
            start_seq,
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        if let Err(e) = self.store.save_run(&run) {
            if !queued {
                self.host.release_turn_reservation(session_id, &run_id);
                self.release_capacity(&run_id);
            }
            let e = OrchError::new(OrchErrorCode::Internal, e.to_string());
            return Err(self.fail_claim(&mut lease, Some(run_id), session_id, &claimed, e));
        }

        let queued_position = if queued {
            match self.enqueue_pending(PendingRun {
                run_id: run_id.clone(),
                session_id,
                prompt: prompt.clone(),
                execution_mode,
            }) {
                Ok(position) => Some(position),
                Err(error) => {
                    let _ = self.store.update_run(&run_id, |current| {
                        current.state = RunState::Failed;
                        current.queue_position = None;
                        current.terminal_result = Some("failed".into());
                        current.error_code = Some(error.code.as_str().into());
                        current.updated_at = Utc::now();
                        Ok(())
                    });
                    return Err(self.fail_claim(
                        &mut lease,
                        Some(run_id),
                        session_id,
                        &claimed,
                        error,
                    ));
                }
            }
        } else {
            None
        };

        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "state": run.state,
            "requestId": request_id,
            "executionMode": execution_mode,
            "queuedPosition": queued_position,
        });
        if let Err(e) = lease.complete(Some(run_id.clone()), response.clone()) {
            let _ = self.store.update_run(&run_id, |r| {
                r.state = RunState::Failed;
                r.terminal_result = Some("failed".into());
                r.error_code = Some("receipt_persistence_failed".into());
                r.updated_at = Utc::now();
                Ok(())
            });
            self.remove_pending(&run_id);
            if !queued {
                self.host.release_turn_reservation(session_id, &run_id);
                self.release_capacity(&run_id);
            }
            return Err(self.fail_claim(&mut lease, Some(run_id), session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            if queued { "run queued" } else { "run started" },
        );
        if !queued {
            self.spawn_run(run, prompt, execution_mode);
        } else {
            // A capacity release can race the enqueue; this also makes an
            // immediately available slot visible without requiring polling.
            self.pump_pending();
        }

        Ok(response)
    }

    /// Explicitly create a bounded replacement for one interrupted run.
    /// Restart recovery never resumes a model turn implicitly; the caller
    /// supplies a fresh prompt and the new request is idempotent on its own.
    #[allow(clippy::too_many_arguments)]
    pub async fn retry_run(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        source_run_id: &str,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: Option<RunExecutionMode>,
        allow_queue: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_retry_run";
        let fail = |svc: &Self, error: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            error
        };
        let source = match self.authorize_run_request(session_id, workspace, source_run_id) {
            Ok(run) => run,
            Err(error) => return Err(fail(self, error)),
        };
        if source.state != RunState::Interrupted {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "only interrupted runs can be explicitly retried",
                ),
            ));
        }

        let previous_mode = source
            .execution
            .as_ref()
            .map(|execution| execution.mode)
            .unwrap_or(RunExecutionMode::Shared);
        if let Some(requested_mode) = execution_mode {
            if requested_mode != previous_mode {
                return Err(fail(
                    self,
                    OrchError::new(
                        OrchErrorCode::InvalidRequest,
                        "a linked retry must preserve the interrupted run execution mode",
                    ),
                ));
            }
        }
        let server_bounds = self.config.lock().bounds.clone();
        if source.bounds.max_prompt_bytes > server_bounds.max_prompt_bytes
            || source.bounds.max_rounds > server_bounds.max_rounds
            || source.bounds.max_duration_ms > server_bounds.max_duration_ms
            || match (
                source.bounds.max_total_tokens,
                server_bounds.max_total_tokens,
            ) {
                (None, Some(_)) => true,
                (Some(source), Some(server)) => source > server,
                _ => false,
            }
        {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "interrupted run exceeds the current retry policy ceiling",
                ),
            ));
        }
        let bounds = match bounds_json {
            Some(value) => {
                let retry_bounds = merge_bounds(&source.bounds, Some(&value))
                    .map_err(|error| fail(self, error))?;
                Some(serde_json::to_value(retry_bounds).map_err(|error| {
                    fail(
                        self,
                        OrchError::new(OrchErrorCode::Internal, error.to_string()),
                    )
                })?)
            }
            None => Some(serde_json::to_value(&source.bounds).map_err(|error| {
                fail(
                    self,
                    OrchError::new(OrchErrorCode::Internal, error.to_string()),
                )
            })?),
        };
        let response = self
            .submit_task_with_execution_mode_and_queue_parent(
                auth,
                request_id,
                session_id,
                workspace,
                prompt,
                bounds,
                previous_mode,
                allow_queue,
                Some(source_run_id),
                tool,
            )
            .await
            .map_err(|error| fail(self, error))?;
        if response["runId"].as_str().is_none() {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::Internal,
                    "retry submission returned no run_id",
                ),
            ));
        }
        let mut response = response;
        response["sourceRunId"] = json!(source_run_id);
        response["retryOf"] = json!(source_run_id);
        response["requestId"] = json!(request_id);
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&source.workspace),
            "accepted",
            None,
            "explicit replacement created for interrupted run",
        );
        Ok(response)
    }

    /// Start a run whose host admission has already been reserved.
    fn spawn_run(&self, run: RunRecord, prompt: String, execution_mode: RunExecutionMode) {
        let host = self.host.clone();
        let store = self.store.clone();
        let bus = self.bus.clone();
        let service_ref = self.self_ref.clone();
        let session_id = run.session_id;
        let rid = run.run_id.clone();
        let max_ms = run.bounds.max_duration_ms;
        let max_rounds = run.bounds.max_rounds;

        // Dedicated aggregator task: must not share a biased select with the
        // duration deadline (chatty ShellOutput must not starve max_duration_ms).
        let mut agg_rx = bus.subscribe();
        let store_agg = store.clone();
        let rid_agg = rid.clone();
        let agg_task = tokio::spawn(async move {
            while let Some(update) = agg_rx.recv().await {
                apply_run_aggregate(&store_agg, &rid_agg, session_id, &update);
            }
        });

        let join = tokio::spawn(async move {
            let admission_guard = AdmissionGuard {
                host: host.clone(),
                run_id: rid.clone(),
            };
            let prompt_fut = host.session_prompt_reserved_with_max_rounds_for_run(
                session_id,
                prompt,
                Some(max_rounds.max(1)),
                &rid,
                &rid,
                execution_mode,
            );
            tokio::pin!(prompt_fut);
            let deadline = tokio::time::sleep(Duration::from_millis(max_ms.max(1)));
            tokio::pin!(deadline);

            // Cancellation and teardown are bounded. A backend that ignores its
            // token cannot hold admission capacity forever.
            let (timed_out, result): (bool, Result<String, anyhow::Error>) = tokio::select! {
                biased;
                _ = &mut deadline => {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        host.cancel_turn_and_await(Some(session_id)),
                    ).await;
                    let settled = tokio::time::timeout(
                        Duration::from_secs(1),
                        &mut prompt_fut,
                    ).await;
                    let result = match settled {
                        Ok(result) => result,
                        Err(_) => Err(anyhow::anyhow!(
                            "turn did not stop within the teardown deadline"
                        )),
                    };
                    (true, result)
                }
                r = &mut prompt_fut => (false, r),
            };

            // Stop aggregator; then reconcile aggregates from the journal range
            // so late FileEdit/test events are not lost if the task was aborted mid-drain.
            agg_task.abort();
            let _ = agg_task.await;

            let end_seq = bus.current_seq();
            let reconciliation = collect_run_updates(&bus, &store, &rid, end_seq);
            let durable_result = match &result {
                Ok(text) => Ok(bus.redact_text(text, 8_000)),
                Err(error) => Err(bus.redact_text(&error.to_string(), 2_000)),
            };
            let mut candidate = store.load_run(&rid).ok().flatten().unwrap_or(run);
            for update in &reconciliation {
                fold_run_update(&mut candidate, update);
            }
            candidate.end_seq = candidate.end_seq.or(Some(end_seq));
            candidate.updated_at = Utc::now();
            if !candidate.state.is_terminal() {
                if timed_out {
                    candidate.state = RunState::LimitReached;
                    candidate.terminal_result = Some("limit_reached".into());
                    candidate.error_code = Some("limit_reached".into());
                    candidate.stop_cause = Some(RunStopCause::DurationLimit);
                    if let Ok(text) = &durable_result {
                        candidate.final_response = Some(text.clone());
                    }
                } else {
                    match &durable_result {
                        Ok(text) => {
                            if candidate
                                .stop_cause
                                .is_some_and(|cause| cause != RunStopCause::Completed)
                            {
                                candidate.state = RunState::LimitReached;
                                let code = candidate
                                    .error_code
                                    .as_deref()
                                    .unwrap_or("limit_reached")
                                    .to_string();
                                candidate.terminal_result = Some(code.clone());
                                candidate.error_code = Some(code);
                            } else {
                                candidate.state = RunState::Completed;
                                candidate.terminal_result = Some("completed".into());
                                candidate.stop_cause = Some(RunStopCause::Completed);
                            }
                            candidate.final_response = Some(text.clone());
                        }
                        Err(error) => {
                            candidate.state = RunState::Failed;
                            candidate.terminal_result = Some("failed".into());
                            candidate.error_code = Some("internal".into());
                            candidate.stop_cause = Some(RunStopCause::Failed);
                            candidate.final_response = Some(error.clone());
                        }
                    }
                }
            }
            // At this point the prompt future has either settled or its
            // bounded teardown window has elapsed. Any remaining provider
            // marker can no longer support a complete-accounting claim.
            candidate.fail_closed_unresolved_provider_attempts();
            if candidate.aggregates.verification.is_none() {
                let observations = crate::completion::observations_from_run(
                    candidate.aggregates.changes.len(),
                    candidate
                        .aggregates
                        .tests
                        .iter()
                        .map(|t| (t.exit_code, t.cancelled)),
                    candidate.aggregates.permissions_requested,
                    candidate.aggregates.permissions_granted,
                    candidate.aggregates.permissions_denied,
                );
                let outcome = candidate.terminal_result.as_deref().unwrap_or("incomplete");
                candidate.aggregates.verification = Some(crate::completion::build_evidence(
                    outcome,
                    candidate.final_response.as_deref(),
                    observations,
                    candidate.aggregates.usage.clone(),
                    matches!(candidate.state, RunState::Cancelled | RunState::Interrupted),
                ));
            }
            // External isolated runs do not pass through the desktop finalizer.
            if let Some(execution) = candidate.execution.as_mut() {
                if execution.mode == RunExecutionMode::IsolatedWorktree {
                    if candidate.state == RunState::Completed {
                        match crate::run_promotion::snapshot(
                            Path::new(&execution.execution_workspace),
                            &execution.base_revision,
                        ) {
                            Ok(snapshot) => {
                                execution.final_fingerprint = Some(snapshot.fingerprint);
                                execution.promotion_state = PromotionState::Ready;
                                if !snapshot.changed_files.is_empty() {
                                    candidate.aggregates.changes = snapshot.changed_files;
                                }
                            }
                            Err(error) => {
                                execution.promotion_state = PromotionState::Conflicted;
                                candidate.error_code = Some("promotion_conflict".into());
                                let _ = store.enqueue_audit(AuditEntry {
                                    ts: Utc::now(),
                                    tool: "run_finalization".into(),
                                    request_id: None,
                                    session_id: Some(session_id),
                                    workspace: Some(candidate.workspace.clone()),
                                    outcome: "promotion_conflict".into(),
                                    error_code: Some("promotion_conflict".into()),
                                    detail: bus.redact_text(&error.to_string(), 500),
                                });
                            }
                        }
                    } else {
                        execution.promotion_state = PromotionState::Conflicted;
                    }
                }
            }
            let mut attempt = 0u32;
            loop {
                let error = match store.persist_finalization(&candidate) {
                    Ok(_) => break,
                    Err(error) => error.to_string(),
                };
                if attempt == 0 {
                    let entry = AuditEntry {
                        ts: Utc::now(),
                        tool: "run_finalization".into(),
                        request_id: None,
                        session_id: Some(session_id),
                        workspace: None,
                        outcome: "retrying".into(),
                        error_code: Some("run_persistence_failed".into()),
                        detail: bus.redact_text(&error, 500),
                    };
                    let _ = store.enqueue_audit(entry);
                    eprintln!("[grokptah] run {rid} finalization retrying: {error}");
                }
                attempt = attempt.saturating_add(1);
                let shift = attempt.min(6);
                let backoff_ms = 25u64.saturating_mul(1u64 << shift).min(1_000);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            }

            // Release capacity before waking the scheduler, so a queued task
            // can be promoted immediately and fairly.
            drop(admission_guard);
            if let Some(service) = service_ref.upgrade() {
                service.pump_pending();
            }
        });
        self.reaping_handles();
        self.join_handles.lock().push(join);
    }

    pub async fn queue_prompt(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        priority: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_queue_prompt";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "priority": priority,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };
        if let Err(e) = reject_control_prompt(&prompt) {
            return Err(fail(self, e));
        }
        if let Err(e) = self.require_build_session(session_id) {
            return Err(fail(self, e));
        }
        let session = self.host.session_inspect(session_id).unwrap();
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let (entries, changed_entry, revision) =
            match self.host.session_queue_add_with_source_receipt(
                session_id,
                prompt,
                priority,
                "control",
                Some("mcp".into()),
            ) {
                Ok(e) => e,
                Err(e) => {
                    return Err(self.fail_claim(
                        &mut lease,
                        None,
                        session_id,
                        &claimed,
                        OrchError::new(OrchErrorCode::Internal, e.to_string()),
                    ));
                }
            };
        let response = json!({
            "requestId": request_id,
            "actionId": request_id,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "origin": "mcp",
            "action": "queued",
            "disposition": "queued",
            "actionVersion": changed_entry.version,
            "revision": revision,
            "entry": changed_entry,
            "entries": entries,
        });
        if let Err(e) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queued",
        );
        Ok(response)
    }

    pub async fn steer(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        text: String,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_steer";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "text": text,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };
        if let Err(e) = reject_control_prompt(&text) {
            return Err(fail(self, e));
        }
        if let Err(e) = self.require_build_session(session_id) {
            return Err(fail(self, e));
        }
        let session = self.host.session_inspect(session_id).unwrap();
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let (receipt, revision) =
            match self
                .host
                .session_steer_with_owner(session_id, text, Some("mcp".into()))
            {
                Ok(r) => r,
                Err(e) => {
                    return Err(self.fail_claim(
                        &mut lease,
                        None,
                        session_id,
                        &claimed,
                        OrchError::new(OrchErrorCode::Internal, e.to_string()),
                    ));
                }
            };
        let response = json!({
            "requestId": request_id,
            "actionId": request_id,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "origin": "mcp",
            "action": "steer_now",
            "disposition": receipt.disposition,
            "entry": receipt.entry,
            "actionVersion": receipt.entry.version,
            "revision": revision,
            "entries": receipt.entries,
        });
        if let Err(e) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "steer",
        );
        Ok(response)
    }

    pub async fn cancel(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: Option<&str>,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_cancel";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };

        let rid = match run_id {
            Some(r) if !r.is_empty() => r,
            _ => {
                return Err(fail(
                    self,
                    OrchError::new(
                        OrchErrorCode::InvalidRequest,
                        "run_id is required for cancel",
                    ),
                ));
            }
        };

        let run = match self.store.load_run(rid) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"),
                ));
            }
            Err(e) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::Internal, e.to_string()),
                ));
            }
        };

        if run.session_id != session_id {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "run_id does not belong to session",
                ),
            ));
        }
        let session = match self.host.session_inspect(session_id) {
            Ok(s) => s,
            Err(_) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"),
                ));
            }
        };
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };
        // Workspace must match the run record as well.
        if claimed.display().to_string() != run.workspace
            && canonical_cmp(&claimed, Path::new(&run.workspace)).is_err()
        {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::WorkspaceMismatch,
                    "workspace does not match run",
                ),
            ));
        }

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        if run.state.is_terminal() {
            let error = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!("run already terminal ({:?})", run.state),
            );
            return Err(self.fail_claim(&mut lease, Some(rid.into()), session_id, &claimed, error));
        }

        // Persist cancelled transactionally before signalling. The closure
        // rechecks state so a concurrent completion can never be overwritten.
        let cancel_update = self.store.update_run(rid, |current| {
            if current.session_id != session_id {
                return Err(anyhow::anyhow!("run_id does not belong to session"));
            }
            if current.state.is_terminal() {
                return Err(anyhow::anyhow!(
                    "run already terminal ({:?})",
                    current.state
                ));
            }
            current.state = RunState::Cancelled;
            current.queue_position = None;
            current.updated_at = Utc::now();
            current.end_seq = None;
            current.terminal_result = Some("cancelled".into());
            current.error_code = Some("cancelled".into());
            current.stop_cause = Some(RunStopCause::Cancelled);
            Ok(())
        });
        if !matches!(cancel_update, Ok(Some(_))) {
            let message = match cancel_update {
                Ok(None) => "run record disappeared during cancel".into(),
                Err(error) => error.to_string(),
                Ok(Some(_)) => unreachable!(),
            };
            return Err(self.fail_claim(
                &mut lease,
                Some(rid.into()),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, message),
            ));
        }

        let was_pending = self.remove_pending(rid);
        let reservation_released = self.host.release_turn_reservation(session_id, rid);
        let teardown_complete = if was_pending || reservation_released {
            true
        } else {
            tokio::time::timeout(Duration::from_secs(5), async {
                let _ = self.host.cancel_turn_and_await(Some(session_id)).await;
                self.host.wait_turn_idle(session_id).await;
            })
            .await
            .is_ok()
        };

        // A completed teardown should have reconciled every provider marker.
        // If it did not—or teardown itself timed out—persist incomplete usage
        // before returning the terminal cancellation receipt.
        let accounting_update = self.store.update_run(rid, |current| {
            current.fail_closed_unresolved_provider_attempts();
            current.updated_at = Utc::now();
            Ok(())
        });
        if !matches!(accounting_update, Ok(Some(_))) {
            let message = match accounting_update {
                Ok(None) => "run record disappeared during cancellation teardown".into(),
                Err(error) => error.to_string(),
                Ok(Some(_)) => unreachable!(),
            };
            return Err(self.fail_claim(
                &mut lease,
                Some(rid.into()),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, message),
            ));
        }

        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "runId": rid,
            "cancelled": true,
            "wasQueued": was_pending,
            "teardownComplete": teardown_complete,
            "state": RunState::Cancelled,
        });
        if let Err(e) = lease.complete(Some(rid.into()), response.clone()) {
            return Err(self.fail_claim(&mut lease, Some(rid.into()), session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "cancelled",
        );
        if was_pending {
            self.pump_pending();
        }
        Ok(response)
    }
}

/// Map a Computer Run read failure into the control plane's error vocabulary.
/// `Unauthorized` covers unknown, cross-session, cross-workspace, unbound, and
/// traversal-shaped reads with one shared message, so this mapping must stay
/// single-valued to preserve that indistinguishability on the wire.
fn computer_scope_denied() -> OrchError {
    OrchError::new(
        OrchErrorCode::ForbiddenScope,
        "computer run is not available to this session",
    )
}

fn computer_read_error(error: crate::computer_use::ComputerError) -> OrchError {
    use crate::computer_use::ComputerErrorCode;
    let code = match error.code {
        ComputerErrorCode::Unauthorized => OrchErrorCode::ForbiddenScope,
        ComputerErrorCode::InvalidRequest => OrchErrorCode::InvalidRequest,
        _ => OrchErrorCode::Internal,
    };
    OrchError::new(code, error.message)
}

fn canonical_cmp(a: &Path, b: &Path) -> Result<(), ()> {
    let ca = dunce::canonicalize(a).map_err(|_| ())?;
    let cb = dunce::canonicalize(b).map_err(|_| ())?;
    if ca == cb {
        Ok(())
    } else {
        Err(())
    }
}

/// Incrementally persist run-scoped aggregates so journal rollover cannot erase them.
pub(crate) fn apply_run_aggregate(
    store: &OrchStore,
    run_id: &str,
    session_id: Uuid,
    update: &crate::events::SessionUpdate,
) {
    if session_id_of(update) != Some(session_id) {
        return;
    }
    if !matches!(
        update,
        crate::events::SessionUpdate::FileEdit { .. }
            | crate::events::SessionUpdate::ShellSessionStarted { .. }
            | crate::events::SessionUpdate::ShellSessionEnded { .. }
            | crate::events::SessionUpdate::AgentProgress { .. }
            | crate::events::SessionUpdate::CompletionEvidence { .. }
    ) {
        return;
    }
    let _ = store.update_run(run_id, |r| {
        if fold_run_update(r, update) {
            r.updated_at = Utc::now();
        }
        Ok(())
    });
}

fn fold_run_update(run: &mut RunRecord, update: &crate::events::SessionUpdate) -> bool {
    match update {
        crate::events::SessionUpdate::FileEdit { path, summary, .. } => {
            if run.aggregates.changes.iter().any(|c| c.path == *path) {
                return false;
            }
            run.aggregates.changes.push(ChangeRecord {
                path: path.clone(),
                summary: summary.clone(),
            });
            true
        }
        crate::events::SessionUpdate::ShellSessionStarted {
            command, call_id, ..
        } if is_recognized_test_command(command) => {
            if run.aggregates.tests.iter().any(|t| t.call_id == *call_id) {
                return false;
            }
            run.aggregates.tests.push(TestObservation {
                call_id: call_id.clone(),
                command: Some(command.clone()),
                status: "started".into(),
                exit_code: None,
                cancelled: None,
            });
            true
        }
        crate::events::SessionUpdate::ShellSessionEnded {
            call_id,
            exit_code,
            cancelled,
            ..
        } => {
            if let Some(t) = run
                .aggregates
                .tests
                .iter_mut()
                .find(|t| t.call_id == *call_id)
            {
                t.status = "ended".into();
                t.exit_code = *exit_code;
                t.cancelled = Some(*cancelled);
                true
            } else {
                false
            }
        }
        crate::events::SessionUpdate::AgentProgress {
            round,
            max_rounds,
            last_tool,
            detail,
            ..
        } => {
            run.progress = Some(RunProgress {
                round: *round,
                max_rounds: *max_rounds,
                last_tool: last_tool.clone(),
                detail: crate::textutil::truncate_at_char_boundary(detail, 2_000).to_string(),
                updated_at: Utc::now(),
            });
            true
        }
        crate::events::SessionUpdate::CompletionEvidence { evidence, .. } => {
            run.aggregates.usage = evidence.usage.clone();
            run.aggregates.permissions_requested = evidence.observations.permissions_requested;
            run.aggregates.permissions_granted = evidence.observations.permissions_granted;
            run.aggregates.permissions_denied = evidence.observations.permissions_denied;
            run.aggregates.verification = Some(evidence.clone());
            true
        }
        _ => false,
    }
}

fn collect_run_updates(
    bus: &EventBus,
    store: &OrchStore,
    run_id: &str,
    end_seq: u64,
) -> Vec<crate::events::SessionUpdate> {
    let Ok(Some(run)) = store.load_run(run_id) else {
        return Vec::new();
    };
    let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
    bus.read_range_all(after, Some(end_seq), Some(run.session_id))
        .map(|entries| entries.into_iter().map(|e| e.update).collect())
        .unwrap_or_default()
}

fn session_id_of(u: &crate::events::SessionUpdate) -> Option<Uuid> {
    use crate::events::SessionUpdate::*;
    match u {
        AgentMessageChunk { session_id, .. }
        | AgentThoughtChunk { session_id, .. }
        | TurnStarted { session_id, .. }
        | ToolCall { session_id, .. }
        | ToolCallUpdate { session_id, .. }
        | Plan { session_id, .. }
        | PermissionRequired { session_id, .. }
        | CompletionEvidence { session_id, .. }
        | TurnComplete { session_id, .. }
        | Error { session_id, .. }
        | SubagentSpawned { session_id, .. }
        | SubagentUpdate { session_id, .. }
        | ShellSessionStarted { session_id, .. }
        | ShellOutput { session_id, .. }
        | ShellSessionEnded { session_id, .. }
        | FileEdit { session_id, .. }
        | AgentProgress { session_id, .. }
        | RateLimited { session_id, .. }
        | SteeringInjected { session_id, .. }
        | PromptQueueChanged { session_id, .. } => Some(*session_id),
        BackgroundTask { session_id, .. } => *session_id,
    }
}
