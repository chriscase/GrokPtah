//! PreToolUse / PostToolUse hooks from project + user hooks.json.
//!
//! Minimal policy layer: matchers can deny tools before they run, and post
//! hooks record observations (logged; optional message returned to the agent).

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::discover::{grokptah_home, hooks_config_text};

#[derive(Debug, Clone, Default, Deserialize)]
struct HooksFile {
    #[serde(default)]
    hooks: HooksInner,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct HooksInner {
    #[serde(default, rename = "PreToolUse")]
    pre_tool_use: Vec<HookEntry>,
    #[serde(default, rename = "PostToolUse")]
    post_tool_use: Vec<HookEntry>,
    /// Stop hooks run at end of a turn (#168).
    #[serde(default, rename = "Stop")]
    stop: Vec<StopHookEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct HookEntry {
    /// Tool name substring or `*` / empty = all tools.
    #[serde(default)]
    matcher: Option<String>,
    /// When true, PreToolUse denies the tool.
    #[serde(default)]
    deny: bool,
    /// Message shown to the agent / UI on deny (or post note).
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StopHookEntry {
    /// When true, keep the agent running and feed `message` back as feedback.
    #[serde(default)]
    continue_turn: bool,
    /// Alias for continue_turn (Build-ish).
    #[serde(default)]
    r#continue: bool,
    /// Feedback text injected for the model when continuing.
    #[serde(default)]
    message: Option<String>,
    /// When true (default for stop without continue), hard-end is allowed.
    #[serde(default)]
    #[allow(dead_code)]
    deny: bool,
}

/// Result of Stop hooks at turn end (#168).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopHookResult {
    /// End the turn normally.
    End,
    /// Continue with feedback for the model (another agent step).
    ContinueWithFeedback(String),
}

fn parse_hooks(project: Option<&Path>) -> HooksInner {
    let raw = hooks_config_text(project);
    serde_json::from_str::<HooksFile>(&raw)
        .map(|f| f.hooks)
        .or_else(|_| {
            // Also accept bare array form under PreToolUse only via full object
            serde_json::from_str::<HooksInner>(&raw)
        })
        .unwrap_or_default()
}

fn matches_tool(matcher: &Option<String>, tool: &str) -> bool {
    match matcher.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("*") => true,
        Some(m) => {
            let m = m.to_ascii_lowercase();
            let t = tool.to_ascii_lowercase();
            t == m || t.contains(&m) || m.contains(&t)
        }
    }
}

/// If a PreToolUse hook denies this tool, return the denial message.
pub fn pre_tool_use_deny(project: Option<&Path>, tool: &str, _input: &Value) -> Option<String> {
    let hooks = parse_hooks(project);
    for h in &hooks.pre_tool_use {
        if !h.deny {
            continue;
        }
        if !matches_tool(&h.matcher, tool) {
            continue;
        }
        let msg = h
            .message
            .clone()
            .unwrap_or_else(|| format!("PreToolUse hook denied `{tool}`"));
        eprintln!("[grokptah] PreToolUse DENY tool={tool}: {msg}");
        return Some(msg);
    }
    None
}

/// Run PostToolUse observers; returns optional notes for diagnostics.
pub fn post_tool_use_note(
    project: Option<&Path>,
    tool: &str,
    status: &str,
    _output: &str,
) -> Option<String> {
    let hooks = parse_hooks(project);
    let mut notes = Vec::new();
    for h in &hooks.post_tool_use {
        if !matches_tool(&h.matcher, tool) {
            continue;
        }
        let msg = h
            .message
            .clone()
            .unwrap_or_else(|| format!("PostToolUse observed `{tool}` ({status})"));
        eprintln!("[grokptah] PostToolUse tool={tool} status={status}: {msg}");
        notes.push(msg);
    }
    if notes.is_empty() {
        None
    } else {
        Some(notes.join("; "))
    }
}

/// Ensure a hooks.json exists for tests / doctor (uses discover seed path).
#[allow(dead_code)]
pub fn ensure_seed_hooks() {
    let _ = hooks_config_text(None);
    let _ = grokptah_home();
}

/// Evaluate Stop hooks after a turn completes (#168).
///
/// First matching continue wins. Hard-end is the default when none continue.
pub fn evaluate_stop_hooks(project: Option<&Path>) -> StopHookResult {
    let hooks = parse_hooks(project);
    for h in &hooks.stop {
        let cont = h.continue_turn || h.r#continue;
        if cont {
            let msg = h
                .message
                .clone()
                .unwrap_or_else(|| "Stop hook requested continue.".into());
            eprintln!("[grokptah] Stop CONTINUE: {msg}");
            return StopHookResult::ContinueWithFeedback(msg);
        }
    }
    StopHookResult::End
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn pre_tool_denies_matching_write() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".grokptah");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(
            cfg.join("hooks.json"),
            r#"{
  "hooks": {
    "PreToolUse": [
      { "matcher": "write_file", "deny": true, "message": "no writes in fixture" }
    ],
    "PostToolUse": []
  }
}"#,
        )
        .unwrap();
        let deny = pre_tool_use_deny(Some(dir.path()), "write_file", &serde_json::json!({}));
        assert_eq!(deny.as_deref(), Some("no writes in fixture"));
        let ok = pre_tool_use_deny(Some(dir.path()), "read_file", &serde_json::json!({}));
        assert!(ok.is_none());
    }

    #[test]
    fn stop_hook_continue_with_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".grokptah");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(
            cfg.join("hooks.json"),
            r#"{
  "hooks": {
    "Stop": [
      { "continue": true, "message": "check tests again" }
    ]
  }
}"#,
        )
        .unwrap();
        match evaluate_stop_hooks(Some(dir.path())) {
            StopHookResult::ContinueWithFeedback(m) => {
                assert!(m.contains("check tests"), "{m}");
            }
            StopHookResult::End => panic!("expected continue"),
        }
    }
}
