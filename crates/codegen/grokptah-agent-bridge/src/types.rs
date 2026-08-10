use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub supports_effort: bool,
    /// Canonical effort values accepted by this model, in UI order.
    pub effort_options: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
}

impl EffortLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubagentIsolationPreference {
    #[default]
    Worktree,
    Shared,
}

impl SubagentIsolationPreference {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::Shared => "shared",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "worktree" => Some(Self::Worktree),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubagentExecutionMode {
    #[default]
    Unknown,
    Worktree,
    ProjectCopy,
    SharedReadOnly,
    SharedMutating,
    IsolationFailed,
}

impl SubagentExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Worktree => "worktree",
            Self::ProjectCopy => "project_copy",
            Self::SharedReadOnly => "shared_read_only",
            Self::SharedMutating => "shared_mutating",
            Self::IsolationFailed => "isolation_failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthState {
    pub signed_in: bool,
    pub display_name: Option<String>,
    pub method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub transport: String,
    pub enabled: bool,
    pub status: String,
}

/// Whether the open project may spawn repo-local MCP stdio servers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpProjectTrust {
    pub project: Option<String>,
    pub has_local_mcp: bool,
    pub trusted: bool,
    /// User already answered yes/no (skip re-prompt until settings change).
    pub decided: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInfo {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: String,
    /// Owning parent Build session (for cancel-one / history filter) (#152).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Completion summary or last progress line (#152).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Last tool name the child used (live panel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<String>,
    /// Actual working directory used by this child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Whether this child is isolated, shared read-only, or shared mutating.
    #[serde(default)]
    pub execution_mode: SubagentExecutionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub id: String,
    pub title: String,
    pub status: String,
    pub scheduled: bool,
    /// `scan` | `shell` | `agent` — kind of long-running work (#52).
    #[serde(default)]
    pub kind: String,
    /// Optional owning session (for adopt / focus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Last progress line (visible outside the transcript scroll).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
