use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: Uuid,
    pub session_id: Uuid,
    /// Durable Run that owns this request when the host has an in-flight turn.
    /// Native managed execution matches this against `ManagedExecutionIntent.runId`
    /// and never guesses from session insertion order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub tool_name: String,
    pub summary: String,
    pub detail: serde_json::Value,
}

/// Non-consuming view of an in-memory host permission oneshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPermissionView {
    pub request_id: Uuid,
    pub session_id: Uuid,
    pub run_id: Option<String>,
    pub tool_name: String,
    pub receiver_open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
    AlwaysAllow,
}

/// Result of consulting global YOLO / per-tool always / allow+deny rules
/// before showing a permission modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGate {
    /// Skip the modal; run the tool.
    AutoAllow,
    /// Skip the modal; refuse the tool (deny wins).
    AutoDeny,
    /// Show the permission modal.
    Prompt,
}

/// Whether a single allow/deny rule matches `tool_name`.
///
/// Rules are simple patterns:
/// - exact tool name (`run_terminal_cmd`)
/// - alias family (`Shell`, `Shell(*)`, `WebFetch(*)` — UI sample format)
/// - substring match (case-insensitive)
/// - `*` matches everything
pub fn rule_matches(rule: &str, tool_name: &str) -> bool {
    let raw = rule.trim();
    if raw.is_empty() {
        return false;
    }
    let r = raw.to_ascii_lowercase();
    let t = tool_name.to_ascii_lowercase();
    if r == "*" || r == "*()" {
        return true;
    }
    // Strip trailing `(…)` from Claude-style `Shell(*)` samples.
    let base = r
        .split_once('(')
        .map(|(b, _)| b.trim())
        .unwrap_or(r.as_str())
        .trim_end_matches('*')
        .trim();
    if base.is_empty() {
        return true;
    }
    // Known family aliases used in the Settings sample + docs.
    let families: &[&str] = match base {
        "shell" | "bash" | "run_terminal_cmd" | "terminal" => &["run_terminal_cmd"],
        "webfetch" | "web_fetch" | "fetch" => &["web_fetch"],
        "write" | "write_file" | "write_files" | "edit" => {
            &["write_file", "write_files", "apply_patch"]
        }
        "apply_patch" | "patch" => &["apply_patch"],
        "read" | "read_file" => &["read_file"],
        "mcp" => &["mcp"],
        other => {
            return t == other || t.contains(other) || other.contains(t.as_str());
        }
    };
    if families.iter().any(|f| t == *f) {
        return true;
    }
    // MCP tools: family "mcp" matches any mcp__* wire name
    if base == "mcp" && t.starts_with("mcp__") {
        return true;
    }
    t.contains(base)
}

/// Deny wins over allow. Always-allow tools / YOLO / bypass skip the modal.
pub fn evaluate_tool_gate(
    tool_name: &str,
    always_approve: bool,
    always_allowed_tools: &std::collections::HashSet<String>,
    permission_mode: &str,
    allow_rules: &[String],
    deny_rules: &[String],
) -> ToolGate {
    // Explicit deny is an operator policy boundary, including in YOLO or
    // bypass mode. Those modes suppress prompts; they do not erase denies.
    if deny_rules.iter().any(|r| rule_matches(r, tool_name)) {
        return ToolGate::AutoDeny;
    }
    if permission_mode == "bypassPermissions" || always_approve {
        return ToolGate::AutoAllow;
    }
    if always_allowed_tools.contains(tool_name) {
        return ToolGate::AutoAllow;
    }
    // MCP: per-tool wire name or blanket "mcp" entry from AlwaysAllow history
    if tool_name.starts_with("mcp__") && always_allowed_tools.contains("mcp") {
        // Legacy blanket — still honor if present, but new AlwaysAllow no longer sets it.
        return ToolGate::AutoAllow;
    }
    if allow_rules.iter().any(|r| rule_matches(r, tool_name)) {
        return ToolGate::AutoAllow;
    }
    ToolGate::Prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn deny_wins_over_allow() {
        let g = evaluate_tool_gate(
            "run_terminal_cmd",
            false,
            &HashSet::new(),
            "default",
            &["Shell(*)".into()],
            &["Shell(*)".into()],
        );
        assert_eq!(g, ToolGate::AutoDeny);
    }

    #[test]
    fn allow_suppresses_prompt() {
        let g = evaluate_tool_gate(
            "run_terminal_cmd",
            false,
            &HashSet::new(),
            "default",
            &["Shell(*)".into()],
            &[],
        );
        assert_eq!(g, ToolGate::AutoAllow);
    }

    #[test]
    fn webfetch_alias_matches_web_fetch() {
        assert!(rule_matches("WebFetch(*)", "web_fetch"));
        assert!(!rule_matches("WebFetch(*)", "run_terminal_cmd"));
    }

    #[test]
    fn yolo_suppresses_prompts_when_no_explicit_deny_exists() {
        let g = evaluate_tool_gate("write_file", true, &HashSet::new(), "default", &[], &[]);
        assert_eq!(g, ToolGate::AutoAllow);
    }

    #[test]
    fn explicit_deny_remains_effective_in_yolo_mode() {
        let g = evaluate_tool_gate(
            "memory_write",
            true,
            &HashSet::new(),
            "default",
            &[],
            &["memory_write".into()],
        );
        assert_eq!(g, ToolGate::AutoDeny);
    }
}
