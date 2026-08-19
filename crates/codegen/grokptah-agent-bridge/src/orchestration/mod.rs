//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod authz;
mod continuation;
mod service;
mod store;
mod supervisor;
mod types;
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
pub(crate) use service::apply_run_aggregate;
pub use service::{OrchestrationConfig, OrchestrationService};
pub use store::{IdempotencyClaim, OrchStore, RetentionPolicy, RetentionReport};
pub use supervisor::{
    WorkloadSupervisor, WorkloadSupervisorStatus, DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL,
};
pub use types::{
    hash_payload, is_recognized_test_command, merge_bounds, prompt_preview, reject_control_prompt,
    safe_id_filename, AgentAuthorityPolicy, AgentLaneAssociation, AgentMemoryPolicy,
    AgentModelSpec, AgentRecord, AgentResumePlan, AgentRuntimeState, AgentSpec, AgentState,
    AuditEntry, ChangeRecord, ContinuationCheckpoint, ContinuationReason, IdempotencyReceipt,
    OrchError, OrchErrorCode, PromotionState, RunAggregates, RunApproval, RunBounds, RunExecution,
    RunExecutionMode, RunProgress, RunRecord, RunState, RunStopCause, TestObservation,
    AGENT_SPEC_SCHEMA_VERSION, CONTROL_TOOLS, DEFAULT_AGENT_TOOL_IDS,
    DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
};
pub use workload::{
    lease_duration, AttemptState, WorkArtifactRef, WorkAttempt, WorkAttemptView, WorkClaim,
    WorkDependency, WorkItem, WorkItemSnapshot, WorkPolicy, WorkProgress, WorkResult,
    WorkRetryPolicy, WorkState, WorkloadReconciliationReport, WORKLOAD_SCHEMA_VERSION,
};
