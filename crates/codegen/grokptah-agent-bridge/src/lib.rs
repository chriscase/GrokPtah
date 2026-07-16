//! In-process agent host for GrokPtah desktop.
//!
//! No child `grok agent stdio` process on the happy path. The host owns
//! sessions, streams typed updates, and completes permission futures from the UI.

mod auth_store;
mod discover;
mod events;
mod host;
mod local_tools;
mod mcp_runtime;
mod models_catalog;
mod permission;
mod project_context;
mod search_engine;
mod session;
mod session_store;
mod types;

pub use discover::{grokptah_home, set_grokptah_home_override};
pub use events::{SessionUpdate, ToolCallKind, ToolCallStatus};
pub use host::{AgentHost, AgentHostHandle, AgentStatus, HostConfig, WorkspaceUiState};
pub use permission::{PermissionDecision, PermissionRequest};
pub use search_engine::{SearchHit, SearchQuery};
pub use session::{SessionKind, SessionSummary, TranscriptEntry};
pub use types::{
    AuthState, BackgroundTask, EffortLevel, McpServerInfo, ModelInfo, PluginInfo, SkillInfo,
    SubagentInfo,
};

/// Crate version string for about / diagnostics.
pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name used by desktop chrome.
pub const PRODUCT_NAME: &str = "GrokPtah";

/// Upstream auto-update is disabled for desktop builds.
pub fn desktop_auto_update_enabled() -> bool {
    false
}
