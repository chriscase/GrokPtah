//! In-process agent host for GrokPtah desktop.
//!
//! No child `grok agent stdio` process on the happy path. The host owns
//! sessions, streams typed updates, and completes permission futures from the UI.

mod auth_store;
mod discover;
mod events;
mod hooks;
mod host;
mod local_tools;
mod mcp_runtime;
mod memory;
mod models_catalog;
mod permission;
mod project_context;
mod search_engine;
mod session;
mod session_store;
mod textutil;
mod todo_list;
mod types;

pub use textutil::{truncate_at_char_boundary, truncate_with_marker};

pub use memory::{inject_context as memory_inject_context, list_facts as memory_list_facts, remember as memory_remember};

pub use discover::{
    grokptah_home, is_project_mcp_trusted, project_has_local_mcp_servers, set_grokptah_home_override,
    set_project_mcp_trusted,
};
/// List MCP tools for the project (spawns stdio servers when allowed).
pub use mcp_runtime::list_mcp_tools;
pub use events::{SessionUpdate, ToolCallKind, ToolCallStatus};
pub use host::{
    is_rate_limit_error, AgentHost, AgentHostHandle, AgentStatus, HostConfig, WorkspaceUiState,
};
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
