//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod authz;
mod service;
mod store;
mod types;

pub use authz::{canonical_workspace, AuthContext, WorkspaceAllowlist};
pub use service::{OrchestrationConfig, OrchestrationService};
pub use store::OrchStore;
pub use types::{
    hash_payload, prompt_preview, reject_control_prompt, AuditEntry, CONTROL_TOOLS,
    FORBIDDEN_TOOLS, IdempotencyReceipt, OrchError, OrchErrorCode, RunBounds, RunRecord, RunState,
};
