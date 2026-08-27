//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod authority;
mod authz;
mod continuation;
pub(crate) mod managed;
mod manager;
mod message;
mod provider_attempt;
mod routine;
mod service;
mod store;
mod supervisor;
mod types;
mod worker;
mod workload;

pub use authority::{
    authorize_optional_scope, denied, host_principal, run_principal, PrincipalScope,
    ScopeStrictness, HOST_PRINCIPAL_OWNER_ID, HOST_PRINCIPAL_TOKEN_ID,
};
pub use authz::{
    authenticate_bearer, canonical_workspace, constant_time_eq, require_bearer, AuthContext,
    AuthCredential, WorkspaceAllowlist,
};
pub use continuation::{
    assemble_continuation_context, AgentContinuationPlan, ContinuationAssemblyFailure,
    ContinuationContext, ContinuationFidelity, ContinuationInputSnapshot, ContinuationMemoryFact,
    ContinuationMemoryInput, ContinuationMemoryScope, ContinuationOmission, ContinuationReasonCode,
    ContinuationRunInput, ContinuationTestInput, ContinuationWorkloadRef,
    CONTINUATION_ASSEMBLER_VERSION, CONTINUATION_SCHEMA_VERSION,
};
pub use managed::{
    assemble_managed_run_input, intersect_run_bounds, managed_execution_eligible,
    select_relevant_managed_messages, truncate_utf8_to_bytes, ManagedExecutionIntent,
    ManagedExecutionPolicy, ManagedFinalizationOutcome, ManagedFinalizationRecord,
    ManagedFinalizationStage, ManagedIntentState, ManagedRetryCause, ManagedWorkMode,
    NativeExecutorStatus, DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS, MANAGED_EXECUTION_SCHEMA_VERSION,
    MANAGED_FINALIZATION_SCHEMA_VERSION, MANAGED_TRUNCATION_MARKER,
};
pub use manager::{
    parse_manager_directive, ManagerCoordinationMode, ManagerCoordinationPolicy,
    ManagerDecisionRecord, ManagerDecisionState, ManagerDirective, ManagerDirectiveEnvelope,
    ManagerNotification, ManagerPlan, ManagerPlanState, ManagerStep, ManagerStepSpec,
    ManagerStepState, MANAGER_SCHEMA_VERSION, MAX_MANAGER_DIRECTIVE_BYTES, MAX_MANAGER_IN_FLIGHT,
    MAX_MANAGER_REPLANS, MAX_MANAGER_STEPS,
};
pub use message::{
    message_activation_unsupported, MessageKind, MessagePage, WorkMessage, MAX_MESSAGE_BODY_BYTES,
    MESSAGE_SCHEMA_VERSION,
};
pub use provider_attempt::{
    attempt_id_for, attempt_request_digest, AttemptSendState, AttemptSettlement, ProviderAttempt,
    SettlementBinding, SettlementEvidence, SettlementOutcome, MAX_SETTLEMENT_NOTE_BYTES,
    PROVIDER_ATTEMPT_SCHEMA_VERSION,
};
pub use routine::{
    occurrence_dedupe_key, ActivationCause, ActivationDisposition, ActivationRecord,
    ActivationRequest, CapturedActivationPolicy, Clock, ExternalAdapterKind, FakeClock,
    MissedRunPolicy, OverlapPolicy, RoutineConcurrencyPolicy, RoutineFireReport, RoutineLifecycle,
    RoutineRecord, RoutineRetryPolicy, RoutineSnapshot, RoutineTrigger, SystemClock, WorkTemplate,
    ROUTINE_SCHEMA_VERSION,
};
pub(crate) use service::apply_run_aggregate;
pub use service::{OrchestrationConfig, OrchestrationService, ReconcileRequest};
pub(crate) use store::workspaces_match;
pub use store::{
    IdempotencyClaim, OrchStore, ProviderAttemptPage, ReceiptPage, ReceiptSummary, RetentionPolicy,
    RetentionReport, MAX_PROVIDER_ATTEMPT_PAGE, MAX_PROVIDER_ATTEMPT_SCAN, MAX_RECEIPT_PAGE,
    MAX_RECEIPT_SCAN,
};
pub use supervisor::{
    ManagerSupervisorReport, ManagerSupervisorStatus, RoutineSupervisor, RoutineSupervisorStatus,
    WorkloadSupervisor, WorkloadSupervisorStatus, DEFAULT_MANAGER_TICK_INTERVAL,
    DEFAULT_ROUTINE_TICK_INTERVAL, DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL,
    MAX_MANAGER_OBSERVATIONS_PER_PASS, MAX_MANAGER_PLANS_PER_PASS,
};
pub use types::{
    hash_payload, is_recognized_test_command, merge_bounds, prompt_preview, reject_control_prompt,
    safe_id_filename, AgentAuthorityPolicy, AgentLaneAssociation, AgentMemoryPolicy,
    AgentModelSpec, AgentRecord, AgentResumePlan, AgentRuntimeState, AgentSpec, AgentState,
    AuditEntry, ChangeRecord, ContinuationCheckpoint, ContinuationReason, IdempotencyReceipt,
    OrchError, OrchErrorCode, PromotionState, RunAggregates, RunApproval, RunBounds, RunExecution,
    RunExecutionMode, RunProgress, RunPurpose, RunRecord, RunState, RunStopCause, TestObservation,
    AGENT_SPEC_SCHEMA_VERSION, CONTROL_TOOLS, DEFAULT_AGENT_TOOL_IDS,
    DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
};
pub use worker::{
    reject_privilege_amplification, MeasuredCapability, WorkerHostKind, WorkerLivenessState,
    WorkerPresence, WorkerProjection, DEFAULT_WORKER_STALE_AFTER_MS,
};
pub use workload::{
    lease_duration, AssignmentStatus, AttemptState, WorkApproval, WorkArtifactRef, WorkAttempt,
    WorkAttemptView, WorkClaim, WorkDecision, WorkDecisionAction, WorkDependency, WorkItem,
    WorkItemSnapshot, WorkPolicy, WorkProgress, WorkResult, WorkRetryPolicy, WorkState,
    WorkloadReconciliationReport, WORKLOAD_SCHEMA_VERSION,
};
