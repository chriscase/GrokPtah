//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod authz;
mod service;
mod store;
mod types;

pub use authz::{canonical_workspace, constant_time_eq, AuthContext, WorkspaceAllowlist};
pub(crate) use service::apply_run_aggregate;
pub use service::{OrchestrationConfig, OrchestrationService};
pub use store::{IdempotencyClaim, OrchStore, RetentionPolicy, RetentionReport};
pub use types::{
    hash_payload, is_recognized_test_command, merge_bounds, prompt_preview, reject_control_prompt,
    safe_id_filename, AgentRecord, AgentResumePlan, AgentState, AuditEntry, ChangeRecord,
    ContinuationCheckpoint, ContinuationReason, IdempotencyReceipt, OrchError, OrchErrorCode,
    PromotionState, RollbackGuarantee, RunAggregates, RunApproval, RunBounds, RunExecution,
    RunExecutionMode, RunRecord, RunState, TestObservation, CONTROL_TOOLS, FORBIDDEN_TOOLS,
    MAX_AGENT_CONTEXT_BYTES,
};
