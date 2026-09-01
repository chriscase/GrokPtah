//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod authz;
mod continuation;
mod graph;
pub(crate) mod managed;
mod manager;
mod message;
mod public_event;
mod public_run;
mod routine;
mod service;
mod store;
mod supervisor;
mod types;
mod worker;
mod workload;

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
pub use graph::{
    compare_work_order, evaluate_admission, order_work, project_scoped_graph,
    resolve_dependency_states, validate_scoped_dependency_graph, AdmissionBlock, DependencyStates,
    GraphScope, WorkGraphNode, MAX_GRAPH_EDGES, MAX_GRAPH_SCOPE_ITEMS,
};
pub use managed::{
    assemble_managed_run_input, intersect_run_bounds, managed_execution_eligible,
    select_relevant_managed_messages, truncate_utf8_to_bytes, ManagedExecutionBudgetProfile,
    ManagedExecutionIntent, ManagedExecutionPolicy, ManagedExecutorKind,
    ManagedFinalizationOutcome, ManagedFinalizationRecord, ManagedFinalizationStage,
    ManagedGrokBudgetLimits, ManagedGrokCliPermissionMode, ManagedGrokInvocation,
    ManagedIntentState, ManagedRetryCause, ManagedWorkMode, NativeExecutorStatus,
    DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS, MANAGED_EXECUTION_SCHEMA_VERSION,
    MANAGED_FINALIZATION_SCHEMA_VERSION, MANAGED_GROK_INVOCATION_SCHEMA_VERSION,
    MANAGED_TRUNCATION_MARKER,
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
pub use public_event::{
    parse_public_event_page_v1, parse_public_event_v1, PublicEventDtoError, PublicEventKindV1,
    PublicEventPageV1, PublicEventV1, PUBLIC_EVENT_SCHEMA_VERSION,
};
pub use public_run::{
    parse_public_run_handoff_v1, parse_public_run_list_v1, parse_public_run_progress_v1,
    parse_public_run_v1, PublicRunDtoError, PublicRunHandoffV1, PublicRunListV1,
    PublicRunProgressV1, PublicRunV1, PUBLIC_RUN_SCHEMA_VERSION,
};
pub use routine::{
    occurrence_dedupe_key, ActivationCause, ActivationDisposition, ActivationRecord,
    ActivationRequest, CapturedActivationPolicy, Clock, ExternalAdapterKind, FakeClock,
    MissedRunPolicy, OverlapPolicy, RoutineConcurrencyPolicy, RoutineFireReport, RoutineLifecycle,
    RoutineRecord, RoutineRetryPolicy, RoutineSnapshot, RoutineTrigger, SystemClock, WorkTemplate,
    ROUTINE_SCHEMA_VERSION,
};
pub(crate) use service::apply_run_aggregate;
pub use service::{ManagedGrokExecutorConfig, OrchestrationConfig, OrchestrationService};
pub(crate) use store::workspaces_match;
pub(crate) use store::AuditWriterStopReport;
pub use store::{IdempotencyClaim, OrchStore, RetentionPolicy, RetentionReport};
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
    AuditEntry, ChangeRecord, ContinuationCheckpoint, ContinuationReason, CoordinatorAgentView,
    CoordinatorCheckpointView, CoordinatorResumePlanView, IdempotencyReceipt, OrchError,
    OrchErrorCode, PromotionState, RunAggregates, RunApproval, RunBounds, RunExecution,
    RunExecutionMode, RunProgress, RunPurpose, RunRecord, RunState, RunStopCause, TestObservation,
    AGENT_SPEC_SCHEMA_VERSION, CONTROL_TOOLS, DEFAULT_AGENT_TOOL_IDS,
    DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
};
pub use worker::{
    reject_privilege_amplification, MeasuredCapability, WorkerHostKind, WorkerLivenessState,
    WorkerObservatoryProjection, WorkerPresence, WorkerProjection, DEFAULT_WORKER_STALE_AFTER_MS,
};
pub use workload::{
    lease_duration, normalize_allowed_file_path, normalize_allowed_files, AssignmentStatus,
    AttemptState, BlockProvenance, WorkApproval, WorkArtifactRef, WorkAttempt, WorkAttemptView,
    WorkClaim, WorkDecision, WorkDecisionAction, WorkDependency, WorkItem, WorkItemSnapshot,
    WorkPolicy, WorkProgress, WorkResult, WorkRetryPolicy, WorkState, WorkloadReconciliationReport,
    MAX_WORK_ALLOWED_FILES, MAX_WORK_ALLOWED_FILE_PATH_BYTES, WORKLOAD_SCHEMA_VERSION,
};
