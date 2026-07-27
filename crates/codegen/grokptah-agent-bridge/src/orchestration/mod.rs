//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod authz;
mod service;
mod store;
mod types;

pub use authz::{canonical_workspace, constant_time_eq, AuthContext, WorkspaceAllowlist};
pub use service::{OrchestrationConfig, OrchestrationService};
pub use store::OrchStore;
pub use types::{
    hash_payload, is_recognized_test_command, merge_bounds, prompt_preview, reject_control_prompt,
    safe_id_filename, AuditEntry, IdempotencyReceipt, OrchError, OrchErrorCode, RunAggregates,
    RunBounds, RunRecord, RunState, CONTROL_TOOLS, FORBIDDEN_TOOLS,
};
