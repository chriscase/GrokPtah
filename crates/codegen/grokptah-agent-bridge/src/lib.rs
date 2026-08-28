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
pub mod eval_oracle;
pub mod eval_report;
pub mod event_bus;
mod events;
mod exec_risk;
mod gateway_config;
mod hooks;
mod host;
mod host_authority;
mod host_helpers;
mod instance_lock;
mod isolation;
mod lane;
pub mod live_attestation;
mod local_tools;
pub mod mcp_control;
pub mod mcp_control_client;
mod mcp_runtime;
mod memory;
mod models_catalog;
pub mod orchestration;
mod permission;
mod process_tree;
mod project_context;
mod prompt_combine;
mod prompt_queue;
mod provider_discovery;
pub mod provider_observation;
mod provider_qualification;
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
pub use prompt_combine::{combine_prefix_len, join_texts, CombineGate};
pub use prompt_queue::{
    PromptQueueBatch, PromptQueueEntry, PromptQueueRunNextResult, PromptQueueSnapshot,
    PromptQueueTakeResult, SteeringDisposition, SteeringReceipt,
};
pub use provider_discovery::{discover_profile_models, parse_compatible_model_catalog};
pub use provider_qualification::{
    qualify_provider_model, ProviderQualificationReport, QualificationCheck, QualificationStatus,
};
pub use ssrf::{check_url as ssrf_check_url, SsrfDecision};
/// Safe public projection of the host-owned provider-attempt ledger.
pub use xai_provider_attempt::{AttemptProjection, SendState as ProviderSendState};

pub use textutil::{truncate_at_char_boundary, truncate_with_marker};

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
    canonical_workspace_string, project_run_at, ActionClass, ActionGrant, ActionGrantSummary,
    ActionOutcome, ActionOutcomeSummary, ComputerAction, ComputerAuditEntry, ComputerBackend,
    ComputerCapabilities, ComputerControlDisposition, ComputerError, ComputerErrorCode,
    ComputerErrorSummary, ComputerObservation, ComputerObservationPlatform, ComputerPermission,
    ComputerPermissionStatus, ComputerPlatformStatus, ComputerPolicy, ComputerReadBinding,
    ComputerRun, ComputerRunCapacity, ComputerRunEventPage, ComputerRunEventRange,
    ComputerRunProgress, ComputerRunProjection, ComputerRunReads, ComputerRunState,
    ComputerScopeCapacity, ComputerStore, ComputerTarget, ComputerTargetCandidate,
    ComputerTargetSummary, ComputerUseLimits, ComputerUseService, GrantIssuer,
    MacOsObservationPlatform, ObservationSummary, SemanticAction, SimulatorBackend,
    DEFAULT_EVENT_PAGE, MAX_EVENT_PAGE,
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
pub use orchestration::{
    is_recognized_test_command, merge_bounds, prompt_preview, safe_id_filename,
    ActivationDisposition, ActivationRecord, AgentAuthorityPolicy, AgentContinuationPlan,
    AgentLaneAssociation, AgentMemoryPolicy, AgentModelSpec, AgentRecord, AgentResumePlan,
    AgentRuntimeState, AgentSpec, AgentState, AuthContext, AuthCredential,
    ContinuationAssemblyFailure, ContinuationCheckpoint, ContinuationContext, ContinuationFidelity,
    ContinuationInputSnapshot, ContinuationMemoryFact, ContinuationMemoryInput,
    ContinuationMemoryScope, ContinuationOmission, ContinuationReason, ContinuationReasonCode,
    ContinuationRunInput, ContinuationTestInput, ContinuationWorkloadRef, FakeClock,
    ManagedExecutionPolicy, MissedRunPolicy, NativeExecutorStatus, OrchStore, OrchestrationConfig,
    OrchestrationService, PromotionState, RetentionPolicy, RetentionReport,
    RoutineConcurrencyPolicy, RoutineLifecycle, RoutineRecord, RoutineRetryPolicy, RoutineSnapshot,
    RoutineTrigger, RunApproval, RunBounds, RunExecution, RunExecutionMode, RunRecord, RunState,
    RunStopCause, WorkAttemptView, WorkDecision, WorkItem, WorkItemSnapshot, WorkMessage,
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
