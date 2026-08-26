//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.

mod admission;
mod authz;
mod ledger_io;
mod projection;
mod seal;
mod service;
mod store;
mod types;

pub use admission::{
    AcceptanceIntent, AttemptLease, AttemptLeaseState, AuthorizationDrift, AuthorizationSnapshot,
    ProviderSendFailure, ProviderSendRecord, ProviderSendState, SealedBounds, SealedTombstone,
    SpecBinding, SpecHolder, StartGate, TerminationOutcome, ACCEPTANCE_INTENT_VERSION,
    ATTEMPT_LEASE_VERSION, DEFAULT_ATTEMPT_LEASE_TTL_MS, DEFAULT_TEARDOWN_BUDGET,
    MAX_INTENT_PROMPT_BYTES, PROVIDER_SEND_VERSION, TOMBSTONE_VERSION,
};
pub use admission::{
    ProviderRequestSink, ProviderRequestTicket, RequestPhase, TeardownUncertain,
    TEARDOWN_UNCERTAIN_VERSION,
};
pub use authz::{canonical_workspace, constant_time_eq, AuthContext, WorkspaceAllowlist};
pub use projection::{
    project_admission, AdmissionProjection, AttemptProjectionState, ProviderSendProjectionState,
    PROJECTION_VERSION,
};
pub use seal::{KeyProtection, SealAuthority, SealStamp, SEAL_VERSION};
pub(crate) use service::apply_run_aggregate;
pub use service::{AttemptStatus, LedgerRequestSink, OrchestrationConfig, OrchestrationService};
pub use store::{
    IdempotencyClaim, OrchStore, ProviderRequestRecord, ResealReport, RetentionPolicy,
    RetentionReport,
};
pub use types::{
    hash_payload, is_recognized_test_command, merge_bounds, prompt_preview, reject_control_prompt,
    safe_id_filename, AgentRecord, AgentResumePlan, AgentState, AuditEntry, ChangeRecord,
    ContinuationCheckpoint, ContinuationReason, IdempotencyReceipt, OrchError, OrchErrorCode,
    PromotionState, RunAggregates, RunApproval, RunBounds, RunExecution, RunExecutionMode,
    RunRecord, RunState, TestObservation, CONTROL_TOOLS, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
};
