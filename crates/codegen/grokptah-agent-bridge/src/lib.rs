//! In-process agent host for GrokPtah desktop.
//!
//! No child `grok agent stdio` process on the happy path. The host owns
//! sessions, streams typed updates, and completes permission futures from the UI.

mod agents_personas;
mod auth_store;
pub mod certification;
mod completion;
mod computer_agent;
pub mod computer_use;
mod discover;
pub mod enterprise_review;
pub mod enterprise_review_plan;
pub mod eval_oracle;
pub mod eval_report;
pub mod event_bus;
mod events;
mod exec_risk;
mod gateway_config;
mod hooks;
mod host;
mod host_helpers;
mod instance_lock;
mod isolation;
mod lane;
pub mod live_attestation;
pub mod live_provider_certification;
mod local_tools;
pub mod mcp_control;
pub mod mcp_control_client;
mod mcp_runtime;
mod memory;
pub mod memory_certification;
mod models_catalog;
mod native_coding_readiness;
pub mod operations_drill;
pub mod orchestration;
mod permission;
mod process_tree;
mod project_context;
mod prompt_combine;
mod prompt_queue;
mod provider_discovery;
pub mod provider_observation;
mod provider_qualification;
pub mod provider_quota_receipt;
pub mod reliability_eval;
mod run_promotion;
mod search_engine;
mod session;
mod session_store;
mod spawn_env;
mod sse;
mod ssrf;
mod textutil;
mod todo_list;
mod types;
pub mod ui_review_evidence;
pub mod worker_certification_evidence;
mod worktree_gc;

pub use agents_personas::{
    discover_agents, discover_personas, resolve_agent, resolve_persona, AgentDef, PersonaDef,
};
pub use certification::{
    public_xai_endpoint_fingerprint, scan_value_for_forbidden_data, ArtifactReference,
    AttemptDisposition, CampaignActuals, CampaignBudgets, CampaignIdentity,
    CertificationBoundLimits, CertificationBoundProfile, CertificationCheck, CertificationError,
    CredentialMethodClass, DurableStateEvidence, PersistentAgentCapture, ProviderAttemptEvidence,
    ProviderDialectClass, ProviderIdentity, ProviderRouteClass, StreamFraming, UsageEvidence,
    MAX_CAPTURE_ATTEMPTS, MAX_CAPTURE_BYTES, MAX_CAPTURE_CHECKS, MAX_PROMOTABLE_ARTIFACT_BYTES,
    MAX_RAW_ARTIFACT_BYTES, PERSISTENT_AGENT_CAPTURE_SCHEMA,
};
pub use enterprise_review::{
    admit_enterprise_review, admit_enterprise_review_with_trust, attestation_signing_bytes,
    expected_route_binding_digest, verify_enterprise_gateway_attestation,
    EnterpriseGatewayAttestation, EnterpriseGatewayTrust, EnterpriseModelTier,
    EnterpriseReviewAdmissionError, EnterpriseReviewEvidence, EnterpriseReviewLease,
    EnterpriseReviewPolicy, ENTERPRISE_REVIEW_ATTESTATION_SCHEMA,
    ENTERPRISE_REVIEW_EVIDENCE_SCHEMA, ENTERPRISE_REVIEW_LEASE_SCHEMA,
    ENTERPRISE_REVIEW_TRUST_SCHEMA, MAX_ENTERPRISE_REVIEW_DURATION_MS,
    MAX_ENTERPRISE_REVIEW_REQUESTS, MAX_ENTERPRISE_REVIEW_TOKENS,
};
pub use enterprise_review_plan::{
    build_enterprise_review_plan, build_enterprise_review_plan_with_trust,
    enterprise_review_work_request_id, EnterpriseReviewCheckpoint, EnterpriseReviewFindingRef,
    EnterpriseReviewOutcome, EnterpriseReviewPass, EnterpriseReviewPassKind,
    EnterpriseReviewPassResult, EnterpriseReviewPassStatus, EnterpriseReviewPlan,
    EnterpriseReviewPlanError, EnterpriseReviewRun, EnterpriseReviewWorkItemTemplate,
    EnterpriseReviewWorkPlan, ENTERPRISE_REVIEW_CHECKPOINT_SCHEMA,
    ENTERPRISE_REVIEW_OUTCOME_SCHEMA, ENTERPRISE_REVIEW_PASS_ATTEMPTS,
    ENTERPRISE_REVIEW_PASS_KINDS, ENTERPRISE_REVIEW_PLAN_SCHEMA,
    ENTERPRISE_REVIEW_WORK_PLAN_SCHEMA,
};
pub use exec_risk::{assess_shell_risk, peel_transparent_prefixes, RiskReport, RiskTier};
pub use gateway_config::{
    load as load_gateway_config, model_selection_key, parse_model_selection,
    save as save_gateway_config, CapabilitySource, ComputerUseTier, GatewayConfig,
    ModelCapabilities, ModelSelection, ProviderDeadlineClass, ProviderDialect, ProviderKind,
    ProviderModel, ProviderProfile, ProviderProfileUpdate,
};
pub use isolation::prepare_isolation_cwd;
pub use live_attestation::{
    attest_grok_build_oidc, attest_grok_build_oidc_with_min_validity, AuthFileState,
    ClientPolicyState, LiveAttestationSchema, LiveCredentialAttestation, LiveCredentialClass,
    LiveEndpointClass, LiveIssuerClass, LiveSafetyError, OverrideState, RedirectPolicyClass,
    RefreshEndpointPolicyState, GROK_BUILD_ENDPOINT, MAX_AUTH_JSON_BYTES, XAI_OIDC_ISSUER,
    XAI_OIDC_TOKEN_ENDPOINT,
};
pub use live_provider_certification::{
    expected_evidence_digest, LiveProviderCampaignEvidence, LiveProviderCampaignEvidenceError,
    LIVE_PROVIDER_CAMPAIGN_EVIDENCE_SCHEMA,
};
pub use memory_certification::{
    expected_evidence_digest as expected_memory_evidence_digest, MemoryLongHorizonEvidence,
    MemoryLongHorizonEvidenceError, MEMORY_LONG_HORIZON_EVIDENCE_SCHEMA, REQUIRED_LOGICAL_YEARS,
    REQUIRED_MEMORY_SCOPES,
};
pub use operations_drill::{
    BuildTargetCleanupEvidence, OperationsDrillCheck, OperationsDrillEnvironment,
    OperationsDrillError, OperationsDrillKind, OperationsDrillReport, OPERATIONS_DRILL_SCHEMA,
};
pub use prompt_combine::{combine_prefix_len, join_texts, CombineGate};
pub use prompt_queue::{
    PromptQueueBatch, PromptQueueEntry, PromptQueueRunNextResult, PromptQueueSnapshot,
    PromptQueueTakeResult, SteeringDisposition, SteeringReceipt,
};
pub use provider_discovery::{discover_profile_models, parse_compatible_model_catalog};
pub use provider_qualification::{
    qualify_provider_model, ProviderQualificationReport, QualificationCheck, QualificationStatus,
};
pub use provider_quota_receipt::{
    expected_receipt_digest, ProviderQuotaReceipt, ProviderQuotaReceiptError,
    ProviderQuotaReceiptKind, ProviderQuotaReceiptSet, PROVIDER_QUOTA_RECEIPT_SCHEMA,
    PROVIDER_QUOTA_RECEIPT_SET_SCHEMA,
};
pub use ssrf::{check_url as ssrf_check_url, SsrfDecision};

pub use textutil::{truncate_at_char_boundary, truncate_with_marker};
pub use ui_review_evidence::{
    UiReviewAccessibilityEvidence, UiReviewCadence, UiReviewDisposition, UiReviewEvidence,
    UiReviewEvidenceError, UiReviewFinding, UiReviewSeverity, UiReviewStateEvidence,
    REQUIRED_UI_REVIEW_STATES, UI_REVIEW_EVIDENCE_SCHEMA,
};
pub use worker_certification_evidence::{
    expected_worker_evidence_digest, LongRunningWorkerEvidence, WorkerCertificationEvidenceError,
    WorkerCheckEvidence, WorkerCredentialLifecycleEvidence, REQUIRED_RESTARTS,
    REQUIRED_SOAK_SECONDS, REQUIRED_WORKERS, REQUIRED_WORKER_CHECKS,
    WORKER_CERTIFICATION_EVIDENCE_SCHEMA,
};

/// Coding-agent efficiency guidance (system prompt fragment for multi-file / multi-bug turns).
pub use host_helpers::coding_agent_efficiency_guidance;
/// Post-cargo tight-budget tool gate helpers (#187 multi_bug burn prevention).
pub use host_helpers::{
    is_edit_or_shell_tool, is_post_cargo_explore_only_burn, should_skip_tool_after_cargo_failure,
};
#[doc(hidden)]
pub use host_helpers::{replay_xai_provider_contract_on_loopback, ProviderContractReplay};

pub use memory::{MemoryFact, MemoryScope};

pub use completion::{
    enrich_terminal_handoff, CompletionClaims, CompletionEvidence, CompletionObservations,
    CompletionUsage,
};
pub use computer_agent::{ComputerAgentEligibility, ComputerAgentProposal};
pub use computer_use::{
    canonical_workspace_string, computer_isolated_visual_status,
    macos_background_safe_capability_proof, macos_native_capability_proof,
    macos_native_physical_input_domain, project_run_at, ActionClass, ActionGrant,
    ActionGrantSummary, ActionOutcome, ActionOutcomeSummary, AgentComputerRunRequest,
    ComputerAction, ComputerAttentionPoint, ComputerAttentionTarget, ComputerAuditEntry,
    ComputerAuthorityToken, ComputerBackend, ComputerBackendPublicView,
    ComputerBackgroundSafetyReceipt, ComputerCapabilities, ComputerCapabilityProof,
    ComputerCapabilityTier, ComputerControlDisposition, ComputerEmergencyControlToken,
    ComputerError, ComputerErrorCode, ComputerErrorSummary, ComputerIsolatedVisualBlocker,
    ComputerIsolatedVisualStatus, ComputerKey, ComputerLocalApproval, ComputerLocalAuditEntry,
    ComputerLocalElement, ComputerLocalError, ComputerLocalGrant, ComputerLocalLimits,
    ComputerLocalObservation, ComputerLocalTarget, ComputerObservation,
    ComputerObservationPlatform, ComputerPermission, ComputerPermissionStatus,
    ComputerPlatformStatus, ComputerPolicy, ComputerPrincipal, ComputerReadBinding, ComputerRun,
    ComputerRunCapacity, ComputerRunEventPage, ComputerRunEventRange, ComputerRunProgress,
    ComputerRunProjection, ComputerRunReads, ComputerRunState, ComputerScopeCapacity,
    ComputerStore, ComputerSurfaceBinding, ComputerSurfaceEvent, ComputerTarget,
    ComputerTargetCandidate, ComputerTargetSummary, ComputerUncertainSurfaceLease,
    ComputerUseLimits, ComputerUseService, GrantIssuer, IsolationProofOrigin,
    MacOsObservationPlatform, ObservationAuthority, ObservationSummary, PhysicalInputDomain,
    PointerButton, PointerButtonState, SemanticAction, SimulatorBackend, SurfaceFreshnessFence,
    AGENT_PRINCIPAL_INTEGRATION_BLOCKER, COMPUTER_RECEIPT_SCHEMA_VERSION,
    COMPUTER_RUN_SCHEMA_VERSION, DEFAULT_EVENT_PAGE, FOREGROUND_CONFLICT_DOMAIN_CAPACITY,
    MACOS_BACKGROUND_SAFE_BACKEND_ID, MACOS_INTERRUPTED_BACKEND_ID, MACOS_NATIVE_BACKEND_ID,
    MAX_EVENT_PAGE, SIMULATOR_BACKGROUND_BACKEND_ID, SIMULATOR_FOREGROUND_BACKEND_ID,
    SIMULATOR_ISOLATED_BACKEND_ID,
};
pub use discover::{
    grokptah_home, home_override_serial, is_project_mcp_trusted, project_has_local_mcp_servers,
    set_grokptah_home_override, set_project_mcp_trusted, RuntimeHome,
};
pub use event_bus::{EventBus, EventReceiver, JournalEntry, JournalPage};
pub use events::{SessionUpdate, ToolCallKind, ToolCallStatus};
pub use host::{AgentHost, AgentHostHandle, AgentStatus, HostConfig, WorkspaceUiState};
pub use lane::{LaneSummary, RuntimeConnectionState, RuntimeTarget};
pub use mcp_control::{
    discovered_tool_names, start_control_from_env, start_control_server, start_control_server_with,
    start_control_server_with_bind, ControlServerHandle, ControlServerLimits,
};
pub use mcp_control_client::{
    ListedTool, LiveEventFrame, LiveNotification, McpControlClient, McpEventStream, McpRemoteError,
    PtahEventNotification, PtahRecoveryNotification, RunScope, MAX_LIVE_EVENT_FRAME_BYTES,
};
/// List MCP tools for the project (spawns stdio servers when allowed).
pub use mcp_runtime::list_mcp_tools;
pub use native_coding_readiness::{
    project_for_owner as project_native_coding_readiness, AdmissionEligibility,
    AdmissionReasonCode, ComputerUseAdmission, NativeCodingReadinessProjection, PurposeAdmission,
    QualificationEvidence, DESKTOP_OWNER_ID, NATIVE_CODING_READINESS_SCHEMA,
};
pub use orchestration::{
    is_recognized_test_command, merge_bounds, prompt_preview, safe_id_filename,
    ActivationDisposition, ActivationRecord, AgentAuthorityPolicy, AgentContinuationPlan,
    AgentLaneAssociation, AgentMemoryPolicy, AgentModelSpec, AgentRecord, AgentResumePlan,
    AgentRuntimeState, AgentSpec, AgentState, AuthContext, AuthCredential, ComputerReadGrant,
    ContinuationAssemblyFailure, ContinuationCheckpoint, ContinuationContext, ContinuationFidelity,
    ContinuationInputSnapshot, ContinuationMemoryFact, ContinuationMemoryInput,
    ContinuationMemoryScope, ContinuationOmission, ContinuationReason, ContinuationReasonCode,
    ContinuationRunInput, ContinuationTestInput, ContinuationWorkloadRef, FakeClock,
    ManagedExecutionPolicy, MissedRunPolicy, NativeExecutorStatus, OrchStore, OrchestrationConfig,
    OrchestrationService, PromotionState, PublicProviderExecution, PublicRun, PublicRunPage,
    PublicRunProgress, RetentionPolicy, RetentionReport, RoutineConcurrencyPolicy,
    RoutineLifecycle, RoutineRecord, RoutineRetryPolicy, RoutineSnapshot, RoutineTrigger,
    RunApproval, RunBounds, RunExecution, RunExecutionMode, RunRecord, RunState, RunStopCause,
    RuntimeHostKind, WorkAttemptView, WorkDecision, WorkItem, WorkItemSnapshot, WorkMessage,
    WorkPolicy, WorkTemplate, WorkerProjection, WorkloadReconciliationReport, WorkloadSupervisor,
    WorkloadSupervisorStatus, WorkspaceAllowlist, AGENT_SPEC_SCHEMA_VERSION,
    CONTINUATION_ASSEMBLER_VERSION, CONTINUATION_SCHEMA_VERSION, CONTROL_TOOLS,
    DEFAULT_AGENT_TOOL_IDS, DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS,
    DEFAULT_WORKLOAD_RECONCILIATION_INTERVAL, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
    ROUTINE_SCHEMA_VERSION,
};
pub use permission::{PendingPermissionView, PermissionDecision, PermissionRequest};
pub use run_promotion::RunReview;
pub use search_engine::{SearchHit, SearchQuery};
pub use session::{
    SessionCompletion, SessionKind, SessionSummary, TranscriptEntry, WorkspaceStatus,
};
pub use spawn_env::{scrub_std_command, scrub_tokio_command, CONTROL_SECRET_ENV_KEYS};
pub use types::{
    AuthState, BackgroundTask, EffortLevel, McpProjectTrust, McpServerInfo, ModelInfo, PluginInfo,
    SkillInfo, SubagentExecutionMode, SubagentInfo, SubagentIsolationPreference,
};

/// Crate version string for about / diagnostics.
pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name used by desktop chrome.
pub const PRODUCT_NAME: &str = "GrokPtah";

/// Upstream auto-update is disabled for desktop builds.
pub fn desktop_auto_update_enabled() -> bool {
    false
}

pub use worktree_gc::{
    candidates_older_than, gc_worktrees, gc_worktrees_with_protected, GcReport, DEFAULT_MAX_AGE,
};

pub use host_helpers::is_rate_limit_error;
