//! In-process agent host for GrokPtah desktop.
//!
//! No child `grok agent stdio` process on the happy path. The host owns
//! sessions, streams typed updates, and completes permission futures from the UI.

pub mod account_facts;
mod agents_personas;
mod attempt_binding;
mod provider_transport;
mod request_admission;

/// Attempt-binding helpers exposed for the crash-cut integration suite.
///
/// The send boundary is the most expensive thing in this crate to get wrong,
/// so its rules are exercised from an integration test against the real
/// ledger rather than only from inside the module that defines them.
pub mod attempt_binding_testkit {
    pub use crate::attempt_binding::{
        intent_digest, provider_idempotency_key, reconcile_interrupted, workspace_handle,
    };
}
mod auth_store;
pub mod capability_contract;
mod completion;
mod computer_agent;
pub mod computer_use;
mod discover;
pub mod eval_oracle;
pub mod eval_report;
pub mod event_bus;
mod events;
mod exec_risk;
pub mod external_worker;
mod gateway_config;
mod hooks;
mod host;
mod host_helpers;
mod instance_lock;
mod isolation;
pub mod launch_truth;
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
pub use capability_contract::{
    advertised_capabilities, CapabilityAvailability, CapabilityDescriptor, CapabilitySet,
    CapabilityTier, CAPABILITY_CONTRACT_VERSION,
};
pub use exec_risk::{assess_shell_risk, peel_transparent_prefixes, RiskReport, RiskTier};
pub use gateway_config::{
    load as load_gateway_config, model_selection_key, parse_model_selection,
    save as save_gateway_config, CapabilitySource, ComputerUseTier, GatewayConfig,
    ModelCapabilities, ModelSelection, ProviderDeadlineClass, ProviderDialect, ProviderKind,
    ProviderModel, ProviderProfile, ProviderProfileUpdate,
};
pub use isolation::prepare_isolation_cwd;
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

pub use textutil::{truncate_at_char_boundary, truncate_with_marker};

/// Coding-agent efficiency guidance (system prompt fragment for multi-file / multi-bug turns).
pub use host_helpers::coding_agent_efficiency_guidance;
/// Post-cargo tight-budget tool gate helpers (#187 multi_bug burn prevention).
pub use host_helpers::{
    is_edit_or_shell_tool, is_post_cargo_explore_only_burn, should_skip_tool_after_cargo_failure,
};

pub use memory::{
    inject_context as memory_inject_context, list_facts as memory_list_facts,
    remember as memory_remember,
};

pub use completion::{
    enrich_terminal_handoff, CompletionClaims, CompletionEvidence, CompletionObservations,
    CompletionUsage,
};
pub use computer_agent::{ComputerAgentEligibility, ComputerAgentProposal};
pub use computer_use::{
    canonical_workspace_string, project_run_at, ActionClass, ActionGrant, ActionGrantSummary,
    ActionOutcome, ActionOutcomeSummary, ComputerAction, ComputerAgentObservation,
    ComputerAuditEntry, ComputerBackend, ComputerCapabilities, ComputerClientIdentity,
    ComputerControlDisposition, ComputerError, ComputerErrorCode, ComputerErrorSummary,
    ComputerGrantRequest, ComputerObservation, ComputerObservationPlatform, ComputerPermission,
    ComputerPermissionStatus, ComputerPlatformStatus, ComputerPolicy, ComputerReadBinding,
    ComputerRun, ComputerRunAgentController, ComputerRunCapacity, ComputerRunController,
    ComputerRunEventPage, ComputerRunEventRange, ComputerRunProgress, ComputerRunProjection,
    ComputerRunReads, ComputerRunState, ComputerScopeCapacity, ComputerStore, ComputerTarget,
    ComputerTargetCandidate, ComputerTargetSummary, ComputerUseLimits, ComputerUseService,
    GrantIssuer, MacOsObservationPlatform, ObservationSummary, SemanticAction, SimulatorBackend,
    DEFAULT_EVENT_PAGE, MAX_EVENT_PAGE,
};
pub use discover::{
    grokptah_home, home_override_serial, is_project_mcp_trusted, project_has_local_mcp_servers,
    set_grokptah_home_override, set_project_mcp_trusted,
};
pub use event_bus::{EventBus, EventReceiver, JournalEntry, JournalPage};
pub use events::{SessionUpdate, ToolCallKind, ToolCallStatus};
pub use external_worker::{
    CursorCloudAdapter, ExternalWorkerAdapter, ExternalWorkerAdapterError, ExternalWorkerRegistry,
    CURSOR_CLOUD_API_BASE,
};
pub use host::{AgentHost, AgentHostHandle, AgentStatus, HostConfig, WorkspaceUiState};
pub use mcp_control::{
    discovered_tool_names, start_control_from_env, start_control_server, start_control_server_with,
    ControlServerHandle, ControlServerLimits,
};
pub use mcp_control_client::{
    ListedTool, LiveEventFrame, LiveNotification, McpControlClient, McpEventStream,
    PtahEventNotification, PtahRecoveryNotification, RunScope, MAX_LIVE_EVENT_FRAME_BYTES,
};
/// List MCP tools for the project (spawns stdio servers when allowed).
pub use mcp_runtime::list_mcp_tools;
pub use orchestration::{
    is_recognized_test_command, merge_bounds, prompt_preview, safe_id_filename, AgentRecord,
    AgentResumePlan, AgentState, ContinuationCheckpoint, ContinuationReason, OrchStore,
    OrchestrationConfig, OrchestrationService, PromotionState, RetentionPolicy, RetentionReport,
    RunApproval, RunBounds, RunExecution, RunExecutionMode, RunRecord, RunState,
    WorkspaceAllowlist, CONTROL_TOOLS, FORBIDDEN_TOOLS, MAX_AGENT_CONTEXT_BYTES,
};
pub use permission::{PermissionDecision, PermissionRequest};
pub use run_promotion::{isolation_readiness, IsolationReadiness, RunReview};
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

/// Seams the admitted-send-boundary suite needs, and nothing more.
///
/// These exist so an integration test can exercise the *real* admission,
/// transport, and ledger against a real socket. They deliberately expose no
/// way to construct a request that bypasses admission: `chat_once` goes
/// through exactly the path a Chat turn does, and `register_provenance` is the
/// same registration the host performs when it opens a turn.
#[doc(hidden)]
pub mod test_support {
    use std::path::Path;

    use uuid::Uuid;

    use crate::orchestration::OrchStore;
    use crate::session::SessionKind;

    /// Keeps a session's provenance registered until it is dropped.
    pub type ProvenanceGuard = crate::request_admission::registry::Guard;

    /// The model-selection key for one provider profile and model.
    pub fn model_selection_key(profile_id: &str, model_id: &str) -> String {
        crate::gateway_config::model_selection_key(profile_id, model_id)
    }

    /// Register where a session's provider calls are recorded, as the host
    /// does when it opens a turn.
    pub fn register_provenance(
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        ledger: OrchStore,
    ) -> ProvenanceGuard {
        crate::request_admission::registry::register(
            session_id,
            crate::request_admission::CallProvenance {
                run_id: run_id.to_string(),
                session_id,
                workspace: workspace.display().to_string(),
                tenant: None,
                project: None,
                ledger,
                authority: crate::attempt_binding::initial_authority(),
            },
        )
    }

    /// One chat completion, through the same path a Chat turn takes.
    pub async fn chat_once(session_id: Uuid, model: &str, prompt: &str) -> anyhow::Result<String> {
        crate::host_helpers::call_xai_chat(
            session_id,
            model,
            &[("user".to_string(), prompt.to_string())],
            None,
            Path::new("."),
            SessionKind::Chat,
        )
        .await
    }
}
