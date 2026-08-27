//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod agent_loop;
mod authz;
mod service;
mod store;
mod types;

pub use agent_loop::{
    admit_step, digest_of, project_loop, AttentionGrant, AttentionReason, BudgetDimension,
    DispatchState, EscalationTicket, LoopDisposition, LoopProjection, LoopState, LoopStep,
    ModelTier, PolicyEnvelope, StepClass, StepSignature, StepVerdict, WaitWitness,
    MAX_DIGEST_BYTES, SIGNATURE_HISTORY,
};
pub use authz::{canonical_workspace, constant_time_eq, AuthContext, WorkspaceAllowlist};
pub(crate) use service::apply_run_aggregate;
pub use service::{OrchestrationConfig, OrchestrationService};
pub use store::{IdempotencyClaim, OrchStore, RetentionPolicy, RetentionReport};
pub use types::{
    hash_payload, is_recognized_test_command, merge_bounds, prompt_preview, reject_control_prompt,
    safe_id_filename, AgentRecord, AgentResumePlan, AgentState, AuditEntry, ChangeRecord,
    ContinuationCheckpoint, ContinuationReason, IdempotencyReceipt, OrchError, OrchErrorCode,
    PromotionState, RunAggregates, RunApproval, RunBounds, RunExecution, RunExecutionMode,
    RunRecord, RunState, TestObservation, CONTROL_TOOLS, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
};
