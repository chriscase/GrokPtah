//! Authenticated orchestration control plane (#196).
//!
//! Pure policy + durable records live here; the MCP transport is a thin adapter.
//!
//! Host-only authority, sealed execution specifications, durable leases, and
//! the closed lifecycle live in this module. Public SDK types are redacted
//! projections and cannot admit work.

mod admission;
mod authority;
mod authz;
mod lease;
mod lifecycle;
mod provider_fence;
mod service;
mod spine_persist;
mod store;
mod supervisor;
mod types;

pub use admission::{admit_verified_only, AdmittedWork, DurableAdmission, SendCutTable};
pub use authority::{
    canonical_mac_bytes, derive_grant, mac_over_fields, opaque_principal, parse_bounds_json,
    sha256_hex, unsigned_provider_spec, verify_fields, AcceptedBounds, HostGrant, HostGrantClass,
    InternalExecutionSpec, LiveRevisions, MacKey, Revision, SpineError, VerifiedSpec,
    MAC_DOMAIN_SPEC, MAC_ENCODING_VERSION,
};
pub use authz::{canonical_workspace, constant_time_eq, AuthContext, WorkspaceAllowlist};
pub use lease::{cas_lease, AttemptLease};
pub use lifecycle::{
    transition_lifecycle, transition_send, ExecutionLifecycle, ProviderSendState, SendRecovery,
};
pub use provider_fence::{
    physical_launch, verify_artifact_bytes, FakeCodingWorker, PhysicalArtifactClaim,
    PhysicalLaunchAck, PhysicalLaunchRequest, VerifiedArtifact,
};
pub(crate) use service::apply_run_aggregate;
pub use service::{OrchestrationConfig, OrchestrationService};
pub use spine_persist::{ExecutionRecord, IdempotencyTombstone, ProviderSendRecord, SpinePersist};
pub use store::{IdempotencyClaim, OrchStore, RetentionPolicy, RetentionReport};
pub use supervisor::{QuiescenceProof, Registration, Supervisor};
pub use types::{
    hash_payload, is_recognized_test_command, merge_bounds, prompt_preview, reject_control_prompt,
    safe_id_filename, AgentRecord, AgentResumePlan, AgentState, AuditEntry, ChangeRecord,
    ContinuationCheckpoint, ContinuationReason, IdempotencyReceipt, OrchError, OrchErrorCode,
    PromotionState, RunAggregates, RunApproval, RunBounds, RunExecution, RunExecutionMode,
    RunRecord, RunState, TestObservation, CONTROL_TOOLS, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
};
