//! In-process agent host for GrokPtah desktop.
//!
//! No child `grok agent stdio` process on the happy path. The host owns
//! sessions, streams typed updates, and completes permission futures from the UI.

mod agents_personas;
mod auth_store;
mod discover;
pub mod eval_oracle;
mod events;
mod exec_risk;
mod gateway_config;
mod hooks;
mod host;
mod host_helpers;
mod instance_lock;
mod isolation;
mod local_tools;
mod mcp_runtime;
mod memory;
mod models_catalog;
mod permission;
mod project_context;
mod prompt_combine;
mod search_engine;
mod session;
mod session_store;
mod ssrf;
mod textutil;
mod todo_list;
mod types;
mod worktree_gc;

pub use agents_personas::{
    discover_agents, discover_personas, resolve_agent, resolve_persona, AgentDef, PersonaDef,
};
pub use exec_risk::{assess_shell_risk, peel_transparent_prefixes, RiskReport, RiskTier};
pub use gateway_config::{load as load_gateway_config, save as save_gateway_config, GatewayConfig};
pub use isolation::prepare_isolation_cwd;
pub use prompt_combine::{combine_prefix_len, join_texts, CombineGate};
pub use ssrf::{check_url as ssrf_check_url, SsrfDecision};

pub use textutil::{truncate_at_char_boundary, truncate_with_marker};

pub use memory::{
    inject_context as memory_inject_context, list_facts as memory_list_facts,
    remember as memory_remember,
};

pub use discover::{
    grokptah_home, home_override_serial, is_project_mcp_trusted, project_has_local_mcp_servers,
    set_grokptah_home_override, set_project_mcp_trusted,
};
pub use events::{SessionUpdate, ToolCallKind, ToolCallStatus};
pub use host::{AgentHost, AgentHostHandle, AgentStatus, HostConfig, WorkspaceUiState};
/// List MCP tools for the project (spawns stdio servers when allowed).
pub use mcp_runtime::list_mcp_tools;
pub use permission::{PermissionDecision, PermissionRequest};
pub use search_engine::{SearchHit, SearchQuery};
pub use session::{SessionKind, SessionSummary, TranscriptEntry};
pub use types::{
    AuthState, BackgroundTask, EffortLevel, McpProjectTrust, McpServerInfo, ModelInfo, PluginInfo,
    SkillInfo, SubagentInfo,
};

/// Crate version string for about / diagnostics.
pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name used by desktop chrome.
pub const PRODUCT_NAME: &str = "GrokPtah";

/// Upstream auto-update is disabled for desktop builds.
pub fn desktop_auto_update_enabled() -> bool {
    false
}

pub use worktree_gc::{candidates_older_than, gc_worktrees, GcReport, DEFAULT_MAX_AGE};

pub use host_helpers::is_rate_limit_error;
