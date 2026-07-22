use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct PermissionState {
    /// Global YOLO — must only flip via explicit settings, never AlwaysAllow.
    pub always_approve: bool,
    /// Tools that received AlwaysAllow.
    pub always_allowed_tools: HashSet<String>,
}

/// Record an AlwaysAllow decision for `tool`.
/// BUG (historical): sets `always_approve = true` globally, approving every tool.
pub fn apply_always_allow(state: &mut PermissionState, tool: &str) {
    state.always_approve = true;
    state.always_allowed_tools.insert(tool.to_string());
}

/// Whether `tool` is auto-approved.
pub fn is_auto_approved(state: &PermissionState, tool: &str) -> bool {
    state.always_approve || state.always_allowed_tools.contains(tool)
}
