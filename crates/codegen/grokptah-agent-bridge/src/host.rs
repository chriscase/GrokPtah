//! In-process agent host — the shipped runtime desktop uses.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::events::{SessionUpdate, ToolCallKind, ToolCallStatus};
use crate::local_tools;
use crate::permission::{PermissionDecision, PermissionRequest};
use crate::session::{Session, SessionSummary, TranscriptEntry};
use crate::types::{
    AuthState, BackgroundTask, EffortLevel, McpServerInfo, ModelInfo, PluginInfo, SkillInfo,
    SubagentInfo,
};

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub default_model: String,
    pub default_effort: EffortLevel,
    pub always_approve: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            default_model: "grok-3".into(),
            default_effort: EffortLevel::Medium,
            always_approve: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub running: bool,
    pub project_cwd: Option<String>,
    pub active_session: Option<Uuid>,
    pub always_approve: bool,
    pub model: String,
    pub effort: EffortLevel,
    pub sandbox_profile: String,
    pub appearance: String,
    pub auto_update_enabled: bool,
}

struct PendingPermission {
    tx: oneshot::Sender<PermissionDecision>,
}

struct Inner {
    running: bool,
    project_cwd: Option<PathBuf>,
    sessions: HashMap<Uuid, Session>,
    active_session: Option<Uuid>,
    always_approve: bool,
    always_allowed_tools: HashSet<String>,
    model: String,
    effort: EffortLevel,
    auth: AuthState,
    sandbox_profile: String,
    appearance: String,
    permission_mode: String,
    allow_rules: Vec<String>,
    deny_rules: Vec<String>,
    mcp_servers: Vec<McpServerInfo>,
    plugins: Vec<PluginInfo>,
    skills: Vec<SkillInfo>,
    subagents: Vec<SubagentInfo>,
    background_tasks: Vec<BackgroundTask>,
    pending_permissions: HashMap<Uuid, PendingPermission>,
    /// Per-session turn cancellation so multiple sessions can run concurrently
    /// (Claude Code–style parallel build sessions).
    turn_cancels: HashMap<Uuid, CancellationToken>,
    event_tx: mpsc::UnboundedSender<SessionUpdate>,
    /// Paths the agent wrote/edited this process (for diff review).
    edited_files: Vec<String>,
    /// Live tool shell child — killed by [`AgentHostHandle::cancel_turn`].
    live_shell: local_tools::LiveShellSlot,
}

/// Shared handle used by Tauri state and tests.
#[derive(Clone)]
pub struct AgentHostHandle {
    inner: Arc<Mutex<Inner>>,
    event_rx_factory: Arc<Mutex<Option<mpsc::UnboundedReceiver<SessionUpdate>>>>,
}

pub struct AgentHost;

impl AgentHost {
    /// Create a new host. Events are pulled via [`AgentHostHandle::take_event_receiver`] once.
    pub fn create(config: HostConfig) -> AgentHostHandle {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let model = config.default_model.clone();
        let effort = config.default_effort;
        let always = config.always_approve;
        let auth = crate::auth_store::load_auth_state();
        let mcp_servers = crate::discover::load_mcp_servers(None);
        let plugins = crate::discover::discover_plugins();
        let skills = crate::discover::discover_skills(None);
        let inner = Inner {
            running: false,
            project_cwd: None,
            sessions: HashMap::new(),
            active_session: None,
            always_approve: always,
            always_allowed_tools: HashSet::new(),
            model,
            effort,
            auth,
            sandbox_profile: "workspace-write".into(),
            appearance: "dark".into(),
            permission_mode: "default".into(),
            allow_rules: Vec::new(),
            deny_rules: Vec::new(),
            mcp_servers,
            plugins,
            skills,
            subagents: Vec::new(),
            background_tasks: Vec::new(),
            pending_permissions: HashMap::new(),
            turn_cancels: HashMap::new(),
            event_tx,
            edited_files: Vec::new(),
            live_shell: Arc::new(tokio::sync::Mutex::new(None)),
        };
        AgentHostHandle {
            inner: Arc::new(Mutex::new(inner)),
            event_rx_factory: Arc::new(Mutex::new(Some(event_rx))),
        }
    }
}

impl AgentHostHandle {
    pub fn take_event_receiver(&self) -> Option<mpsc::UnboundedReceiver<SessionUpdate>> {
        self.event_rx_factory.lock().take()
    }

    pub fn status(&self) -> AgentStatus {
        let g = self.inner.lock();
        AgentStatus {
            running: g.running,
            project_cwd: g.project_cwd.as_ref().map(|p| p.display().to_string()),
            active_session: g.active_session,
            always_approve: g.always_approve,
            model: g.model.clone(),
            effort: g.effort,
            sandbox_profile: g.sandbox_profile.clone(),
            appearance: g.appearance.clone(),
            auto_update_enabled: crate::desktop_auto_update_enabled(),
        }
    }

    pub fn start(&self) -> Result<()> {
        self.inner.lock().running = true;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        let mut g = self.inner.lock();
        for (_, c) in g.turn_cancels.drain() {
            c.cancel();
        }
        g.running = false;
        Ok(())
    }

    pub fn set_project_cwd(&self, path: impl AsRef<Path>) -> Result<String> {
        let p = path.as_ref().to_path_buf();
        if !p.is_dir() {
            bail!("not a directory: {}", p.display());
        }
        let mcp = crate::discover::load_mcp_servers(Some(&p));
        let skills = crate::discover::discover_skills(Some(&p));
        let mut g = self.inner.lock();
        g.project_cwd = Some(p.clone());
        g.mcp_servers = mcp;
        g.skills = skills;
        Ok(p.display().to_string())
    }

    pub fn session_new(&self) -> Result<SessionSummary> {
        let mut g = self.inner.lock();
        if !g.running {
            bail!("agent not started");
        }
        let cwd = g
            .project_cwd
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let model = g.model.clone();
        let effort = g.effort;
        let s = Session::new(cwd, model, effort);
        let summary = s.summary();
        g.active_session = Some(s.id);
        g.sessions.insert(s.id, s);
        Ok(summary)
    }

    pub fn session_load(&self, id: Uuid) -> Result<SessionSummary> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        let summary = s.summary();
        g.active_session = Some(id);
        Ok(summary)
    }

    /// Full transcript for hydrating a session tab without replaying events.
    pub fn session_transcript(&self, id: Uuid) -> Result<Vec<TranscriptEntry>> {
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        Ok(s.transcript.clone())
    }

    /// Whether a session currently has an in-flight turn.
    pub fn session_busy(&self, id: Uuid) -> bool {
        self.inner.lock().turn_cancels.contains_key(&id)
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let g = self.inner.lock();
        let mut v: Vec<_> = g.sessions.values().map(|s| s.summary()).collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn fork_session(&self, source: Uuid) -> Result<SessionSummary> {
        let mut g = self.inner.lock();
        let src = g
            .sessions
            .get(&source)
            .ok_or_else(|| anyhow!("unknown session"))?
            .clone();
        let mut s = Session::new(src.cwd.clone(), src.model.clone(), src.effort);
        s.transcript = src.transcript.clone();
        s.title = format!("{} (fork)", src.title);
        s.forked_from = Some(source);
        s.plan_mode = src.plan_mode;
        s.plan_steps = src.plan_steps.clone();
        let summary = s.summary();
        g.active_session = Some(s.id);
        g.sessions.insert(s.id, s);
        Ok(summary)
    }

    pub fn rewind_session(&self, id: Uuid, keep_messages: usize) -> Result<SessionSummary> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        if keep_messages < s.transcript.len() {
            s.transcript.truncate(keep_messages);
        }
        s.updated_at = Utc::now();
        Ok(s.summary())
    }

    pub fn compact_session(&self, id: Uuid) -> Result<SessionSummary> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        if s.transcript.len() > 2 {
            let keep = 2;
            let drop_n = s.transcript.len() - keep;
            s.transcript = s.transcript.split_off(drop_n);
            s.transcript.insert(
                0,
                TranscriptEntry {
                    role: "system".into(),
                    text: format!("[compacted {drop_n} prior messages]"),
                },
            );
            s.compacted_summary = Some(format!("compacted {drop_n}"));
        }
        s.updated_at = Utc::now();
        Ok(s.summary())
    }

    pub fn set_model(&self, model: String) {
        self.inner.lock().model = model;
    }

    pub fn set_effort(&self, effort: EffortLevel) {
        self.inner.lock().effort = effort;
    }

    pub fn set_always_approve(&self, v: bool) {
        self.inner.lock().always_approve = v;
    }

    pub fn set_sandbox(&self, profile: String) {
        self.inner.lock().sandbox_profile = profile;
    }

    pub fn set_appearance(&self, appearance: String) {
        self.inner.lock().appearance = appearance;
    }

    pub fn set_permission_mode(&self, mode: String) {
        self.inner.lock().permission_mode = mode;
    }

    pub fn set_allow_deny_rules(&self, allow: Vec<String>, deny: Vec<String>) {
        let mut g = self.inner.lock();
        g.allow_rules = allow;
        g.deny_rules = deny;
    }

    pub fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "grok-3".into(),
                display_name: "Grok 3".into(),
                supports_effort: true,
            },
            ModelInfo {
                id: "grok-4".into(),
                display_name: "Grok 4".into(),
                supports_effort: true,
            },
            ModelInfo {
                id: "grok-3-mini".into(),
                display_name: "Grok 3 Mini".into(),
                supports_effort: false,
            },
            ModelInfo {
                id: "grok-2".into(),
                display_name: "Grok 2".into(),
                supports_effort: false,
            },
        ]
    }

    pub fn auth_state(&self) -> AuthState {
        // Refresh from keyring/env so external key changes are visible
        let state = crate::auth_store::load_auth_state();
        self.inner.lock().auth = state.clone();
        state
    }

    pub fn sign_in_local(&self, display_name: String) -> AuthState {
        // Local display-only session without API key (still marked signed-in for UI)
        let mut g = self.inner.lock();
        g.auth = AuthState {
            signed_in: true,
            display_name: Some(display_name),
            method: Some("local".into()),
        };
        g.auth.clone()
    }

    pub fn set_api_key(&self, api_key: String, display_name: String) -> Result<AuthState> {
        let state = crate::auth_store::store_api_key(&api_key, &display_name)
            .map_err(|e| anyhow!(e))?;
        self.inner.lock().auth = state.clone();
        Ok(state)
    }

    pub fn open_login(&self) -> Result<String> {
        crate::auth_store::open_login_page().map_err(|e| anyhow!(e))
    }

    pub fn sign_out(&self) -> AuthState {
        let state = crate::auth_store::clear_credentials();
        self.inner.lock().auth = state.clone();
        state
    }

    pub fn mcp_list(&self) -> Vec<McpServerInfo> {
        let project = self.inner.lock().project_cwd.clone();
        let list = crate::discover::load_mcp_servers(project.as_deref());
        self.inner.lock().mcp_servers = list.clone();
        list
    }

    pub fn mcp_set_enabled(&self, name: &str, enabled: bool) -> Result<McpServerInfo> {
        let project = self.inner.lock().project_cwd.clone();
        if !crate::discover::save_mcp_server_enabled(project.as_deref(), name, enabled) {
            // still update in-memory for tests without config file write success
            let mut g = self.inner.lock();
            if let Some(s) = g.mcp_servers.iter_mut().find(|s| s.name == name) {
                s.enabled = enabled;
                s.status = if enabled { "configured".into() } else { "disabled".into() };
                return Ok(s.clone());
            }
            bail!("unknown MCP server");
        }
        let list = crate::discover::load_mcp_servers(project.as_deref());
        let mut g = self.inner.lock();
        g.mcp_servers = list;
        g.mcp_servers
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown MCP server"))
    }

    pub fn mcp_doctor(&self) -> Vec<String> {
        let project = self.inner.lock().project_cwd.clone();
        crate::discover::mcp_doctor_lines(project.as_deref())
    }

    pub fn mcp_add_stdio(&self, name: &str, command: &str, args: Vec<String>) -> Result<()> {
        crate::discover::add_mcp_stdio(name, command, args).map_err(|e| anyhow!(e))?;
        let project = self.inner.lock().project_cwd.clone();
        let list = crate::discover::load_mcp_servers(project.as_deref());
        self.inner.lock().mcp_servers = list;
        Ok(())
    }

    pub fn plugins(&self) -> Vec<PluginInfo> {
        let list = crate::discover::discover_plugins();
        self.inner.lock().plugins = list.clone();
        list
    }

    pub fn plugin_install(&self, id: &str) -> Result<PluginInfo> {
        let p = crate::discover::install_plugin(id).map_err(|e| anyhow!(e))?;
        self.inner.lock().plugins = crate::discover::discover_plugins();
        Ok(p)
    }

    pub fn skills(&self) -> Vec<SkillInfo> {
        let project = self.inner.lock().project_cwd.clone();
        let list = crate::discover::discover_skills(project.as_deref());
        self.inner.lock().skills = list.clone();
        list
    }

    pub fn hooks_config(&self) -> String {
        let project = self.inner.lock().project_cwd.clone();
        crate::discover::hooks_config_text(project.as_deref())
    }

    pub fn agent_edit_diffs(&self) -> Result<String> {
        let (cwd, files) = {
            let g = self.inner.lock();
            (
                g.project_cwd
                    .clone()
                    .ok_or_else(|| anyhow!("no project open"))?,
                g.edited_files.clone(),
            )
        };
        if files.is_empty() {
            // fall back to full git diff
            return self.git_diff();
        }
        let mut out = String::new();
        for f in files {
            let output = std::process::Command::new("git")
                .args(["diff", "HEAD", "--", &f])
                .current_dir(&cwd)
                .output()?;
            out.push_str(&format!("--- {f} ---\n"));
            out.push_str(&String::from_utf8_lossy(&output.stdout));
            out.push('\n');
        }
        Ok(out)
    }

    pub fn record_edit(&self, path: &str) {
        let mut g = self.inner.lock();
        if !g.edited_files.iter().any(|p| p == path) {
            g.edited_files.push(path.to_string());
        }
    }

    pub fn subagents(&self) -> Vec<SubagentInfo> {
        self.inner.lock().subagents.clone()
    }

    pub fn background_tasks(&self) -> Vec<BackgroundTask> {
        self.inner.lock().background_tasks.clone()
    }

    pub fn cancel_background_task(&self, id: &str) -> Result<()> {
        let mut g = self.inner.lock();
        let t = g
            .background_tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("unknown task"))?;
        t.status = "cancelled".into();
        Ok(())
    }

    pub fn schedule_background_task(&self, title: String) -> BackgroundTask {
        let id = Uuid::new_v4().to_string();
        let t = BackgroundTask {
            id: id.clone(),
            title: title.clone(),
            status: "running".into(),
            scheduled: true,
        };
        {
            let mut g = self.inner.lock();
            g.background_tasks.push(t.clone());
        }
        let host = self.clone();
        let task_id = id.clone();
        let event_tx = self.inner.lock().event_tx.clone();
        tokio::spawn(async move {
            // Real async work: walk project and count files (or sleep if no project)
            let cwd = host.inner.lock().project_cwd.clone();
            let result = if let Some(cwd) = cwd {
                tokio::task::spawn_blocking(move || {
                    walkdir::WalkDir::new(cwd)
                        .max_depth(6)
                        .into_iter()
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_type().is_file())
                        .count()
                })
                .await
                .unwrap_or(0)
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                0
            };
            {
                let mut g = host.inner.lock();
                if let Some(task) = g.background_tasks.iter_mut().find(|t| t.id == task_id) {
                    if task.status != "cancelled" {
                        task.status = format!("completed ({result} files)");
                    }
                }
            }
            let _ = event_tx.send(SessionUpdate::BackgroundTask {
                session_id: None,
                task_id: task_id.clone(),
                title,
                status: format!("completed ({result} files)"),
            });
        });
        t
    }

    pub fn fuzzy_open(&self, query: &str) -> Result<Vec<String>> {
        let g = self.inner.lock();
        let cwd = g
            .project_cwd
            .as_ref()
            .ok_or_else(|| anyhow!("no project open"))?;
        Ok(local_tools::fuzzy_files(cwd, query, 40))
    }

    pub fn file_tree(&self) -> Result<Vec<String>> {
        let g = self.inner.lock();
        let cwd = g
            .project_cwd
            .as_ref()
            .ok_or_else(|| anyhow!("no project open"))?;
        Ok(local_tools::list_tree(cwd, 200))
    }

    pub fn git_status(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["status", "--short", "--branch"])
            .current_dir(&cwd)
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn git_diff(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(&cwd)
            .output()?;
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        if s.len() > 64_000 {
            s.truncate(64_000);
            s.push_str("\n…");
        }
        Ok(s)
    }

    pub fn git_stage_all(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&cwd)
            .output()?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr));
        }
        Ok("staged".into())
    }

    pub fn git_commit(&self, message: &str) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&cwd)
            .output()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            bail!("{text}");
        }
        Ok(text)
    }

    pub fn list_worktrees(&self) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let out = std::process::Command::new("git")
            .args(["worktree", "list"])
            .current_dir(&cwd)
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn project_rules(&self) -> Result<Vec<String>> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let candidates = [
            "AGENTS.md",
            "Claude.md",
            "CLAUDE.md",
            ".grok/rules.md",
            "docs/ARCHITECTURE.md",
        ];
        let mut found = Vec::new();
        for c in candidates {
            if cwd.join(c).is_file() {
                found.push(c.to_string());
            }
        }
        Ok(found)
    }

    pub fn settings_snapshot(&self) -> serde_json::Value {
        let g = self.inner.lock();
        serde_json::json!({
            "model": g.model,
            "effort": g.effort,
            "alwaysApprove": g.always_approve,
            "sandboxProfile": g.sandbox_profile,
            "appearance": g.appearance,
            "permissionMode": g.permission_mode,
            "allowRules": g.allow_rules,
            "denyRules": g.deny_rules,
            "autoUpdateEnabled": crate::desktop_auto_update_enabled(),
        })
    }

    pub fn set_plan_mode(&self, session_id: Uuid, enabled: bool) -> Result<()> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        s.plan_mode = enabled;
        Ok(())
    }

    pub fn accept_plan(&self, session_id: Uuid) -> Result<()> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        s.plan_mode = false;
        let steps = s.plan_steps.clone();
        let tx = g.event_tx.clone();
        drop(g);
        let _ = tx.send(SessionUpdate::Plan {
            session_id,
            steps,
            status: "accepted".into(),
        });
        Ok(())
    }

    pub fn reject_plan(&self, session_id: Uuid) -> Result<()> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        s.plan_mode = false;
        s.plan_steps.clear();
        let tx = g.event_tx.clone();
        drop(g);
        let _ = tx.send(SessionUpdate::Plan {
            session_id,
            steps: vec![],
            status: "rejected".into(),
        });
        Ok(())
    }

    pub fn permission_respond(&self, request_id: Uuid, decision: PermissionDecision) -> Result<()> {
        let mut g = self.inner.lock();
        let pending = g
            .pending_permissions
            .remove(&request_id)
            .ok_or_else(|| anyhow!("no pending permission {request_id}"))?;
        if decision == PermissionDecision::AlwaysAllow {
            g.always_approve = true;
        }
        let _ = pending.tx.send(decision);
        Ok(())
    }

    /// Cancel the in-flight turn for `session_id`, or every active turn when
    /// `session_id` is `None` (shutdown / global stop).
    pub fn cancel_turn(&self, session_id: Option<Uuid>) -> Result<()> {
        let live_shell = {
            let g = self.inner.lock();
            match session_id {
                Some(id) => {
                    let Some(c) = g.turn_cancels.get(&id) else {
                        bail!("no active turn for session {id}");
                    };
                    c.cancel();
                }
                None => {
                    if g.turn_cancels.is_empty() {
                        bail!("no active turn");
                    }
                    for c in g.turn_cancels.values() {
                        c.cancel();
                    }
                }
            }
            g.live_shell.clone()
        };
        // Kill live shell child immediately (not only cooperative sleep).
        let handle = tokio::runtime::Handle::try_current();
        if let Ok(h) = handle {
            h.spawn(async move {
                let mut slot = live_shell.lock().await;
                if let Some(mut child) = slot.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            });
        } else {
            // Sync fallback: try_lock and kill
            if let Ok(mut slot) = live_shell.try_lock() {
                if let Some(mut child) = slot.take() {
                    let _ = child.start_kill();
                }
            }
        }
        Ok(())
    }

    /// Run a turn. Returns the final assistant text so the UI always has a
    /// reply even if event delivery is delayed.
    ///
    /// Multiple sessions may run turns concurrently; each keeps its own
    /// cancellation token keyed by `session_id`.
    pub async fn session_prompt(&self, session_id: Uuid, prompt: String) -> Result<String> {
        let (cwd, model, effort, plan_mode, cancel, event_tx) = {
            let mut g = self.inner.lock();
            if !g.running {
                bail!("agent not started");
            }
            // One in-flight turn per session (re-prompt while busy is an error).
            if g.turn_cancels.contains_key(&session_id) {
                bail!("session already has an active turn");
            }
            // Keep session model in sync with host selection
            let model = g.model.clone();
            let effort = g.effort;
            let cancel = CancellationToken::new();
            g.turn_cancels.insert(session_id, cancel.clone());
            g.active_session = Some(session_id);
            let event_tx = g.event_tx.clone();
            let s = g
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.model = model.clone();
            s.effort = effort;
            s.transcript.push(TranscriptEntry {
                role: "user".into(),
                text: prompt.clone(),
            });
            if s.title == "New session" {
                s.title = prompt.chars().take(48).collect();
            }
            s.updated_at = Utc::now();
            (
                s.cwd.clone(),
                model,
                effort,
                s.plan_mode,
                cancel,
                event_tx,
            )
        };

        let result = self
            .run_turn(
                session_id,
                &cwd,
                &model,
                effort,
                plan_mode,
                &prompt,
                cancel.clone(),
                event_tx.clone(),
            )
            .await;

        {
            let mut g = self.inner.lock();
            g.turn_cancels.remove(&session_id);
        }

        let cancelled = cancel.is_cancelled();
        match result {
            Ok(reply) => {
                let _ = event_tx.send(SessionUpdate::TurnComplete {
                    session_id,
                    cancelled,
                });
                Ok(reply)
            }
            Err(e) => {
                let _ = event_tx.send(SessionUpdate::Error {
                    session_id,
                    message: e.to_string(),
                });
                let _ = event_tx.send(SessionUpdate::TurnComplete {
                    session_id,
                    cancelled,
                });
                Err(e)
            }
        }
    }

    async fn run_turn(
        &self,
        session_id: Uuid,
        cwd: &Path,
        model: &str,
        effort: EffortLevel,
        plan_mode: bool,
        prompt: &str,
        cancel: CancellationToken,
        event_tx: mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }

        let auth_label = crate::auth_store::resolve_wire_credentials()
            .map(|w| format!("{} via {}", w.display_name, w.method))
            .unwrap_or_else(|| "no credentials".into());
        emit_thought(
            &event_tx,
            session_id,
            &format!(
                "model={model} effort={} auth={auth_label}",
                effort.as_str()
            ),
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let lower = prompt.to_lowercase();
        if plan_mode || lower.starts_with("/plan") || lower.contains("make a plan") {
            let steps = vec![
                "Explore the relevant files".into(),
                "Draft implementation approach".into(),
                "Apply changes with permissions".into(),
                "Verify with tests".into(),
            ];
            {
                let mut g = self.inner.lock();
                if let Some(s) = g.sessions.get_mut(&session_id) {
                    s.plan_mode = true;
                    s.plan_steps = steps.clone();
                }
            }
            let _ = event_tx.send(SessionUpdate::Plan {
                session_id,
                steps,
                status: "proposed".into(),
            });
            let msg = "Plan proposed. Accept or reject from the plan panel.";
            emit_message(&event_tx, session_id, msg);
            push_assistant(self, session_id, msg);
            return Ok(msg.into());
        }

        if let Some(rest) = prompt.strip_prefix('/') {
            let cmd = rest.split_whitespace().next().unwrap_or("");
            match cmd {
                "help" => {
                    let text =
                        "Commands: /help /model /effort /clear /compact /plan /explore /task /yolo";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                "yolo" => {
                    self.set_always_approve(true);
                    let text = "Always-approve enabled.";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                _ => {}
            }
        }

        if lower.contains("explore") || lower.contains("subagent") {
            let sid = Uuid::new_v4().to_string();
            {
                let mut g = self.inner.lock();
                g.subagents.push(SubagentInfo {
                    id: sid.clone(),
                    kind: "explore".into(),
                    title: "Explore codebase".into(),
                    status: "running".into(),
                });
            }
            let _ = event_tx.send(SessionUpdate::SubagentSpawned {
                session_id,
                subagent_id: sid.clone(),
                kind: "explore".into(),
                title: "Explore codebase".into(),
            });
            let _ = event_tx.send(SessionUpdate::SubagentUpdate {
                session_id,
                subagent_id: sid.clone(),
                status: "completed".into(),
                detail: Some("Indexed project files".into()),
            });
            {
                let mut g = self.inner.lock();
                if let Some(a) = g.subagents.iter_mut().find(|a| a.id == sid) {
                    a.status = "completed".into();
                }
            }
        }

        if lower.contains("background") || lower.contains("schedule") {
            let task = self.schedule_background_task("Background index".into());
            let _ = event_tx.send(SessionUpdate::BackgroundTask {
                session_id: Some(session_id),
                task_id: task.id,
                title: task.title,
                status: task.status,
            });
        }

        if lower.contains("list") || lower.contains("files") || lower.contains("ls ") {
            self.run_tool_call(
                session_id,
                "list_dir",
                || async { local_tools::tool_list_dir(cwd, ".").await },
                &cancel,
                &event_tx,
            )
            .await?;
        }

        if let Some(path) = extract_read_path(prompt) {
            self.run_tool_call(
                session_id,
                "read_file",
                || async { local_tools::tool_read_file(cwd, &path).await },
                &cancel,
                &event_tx,
            )
            .await?;
        }

        if let Some(pat) = extract_search_pattern(prompt) {
            self.run_tool_call(
                session_id,
                "grep",
                || async { local_tools::tool_grep(cwd, &pat, ".").await },
                &cancel,
                &event_tx,
            )
            .await?;
        }

        // "write PATH << content" or "write PATH: content"
        if let Some((wpath, content)) = extract_write(prompt) {
            let host = self.clone();
            let path_for_record = wpath.clone();
            self.run_tool_call(
                session_id,
                "write_file",
                {
                    let cwd = cwd.to_path_buf();
                    let wpath = wpath.clone();
                    let content = content.clone();
                    move || async move { local_tools::tool_write_file(&cwd, &wpath, &content).await }
                },
                &cancel,
                &event_tx,
            )
            .await?;
            host.record_edit(&path_for_record);
        }

        if let Some(cmd) = extract_shell(prompt) {
            self.run_shell_tool(session_id, cwd, &cmd, &cancel, &event_tx)
                .await?;
        }

        if cancel.is_cancelled() {
            emit_message(&event_tx, session_id, "(cancelled)");
            return Ok("(cancelled)".into());
        }

        // Live model when Grok Build session / API key is available.
        let reply = if let Some(creds) = crate::auth_store::resolve_wire_credentials() {
            emit_thought(
                &event_tx,
                session_id,
                &format!("calling xAI chat API as {}…", creds.method),
            );
            match call_xai_chat(&creds, model, prompt, cwd).await {
                Ok(text) => text,
                Err(e) => format!(
                    "Model call failed: {e}\n\n\
                     Auth source: {} ({})\n\
                     Tips: run `grok login` if the CLI session expired, or set XAI_API_KEY / Save key.\n\
                     Project: {}",
                    creds.display_name,
                    creds.method,
                    cwd.display()
                ),
            }
        } else {
            format!(
                "{}\n\nYou said: {}\nProject: {}\nModel: {} · effort: {}",
                crate::auth_store::auth_help_message(),
                prompt.chars().take(200).collect::<String>(),
                cwd.display(),
                model,
                effort.as_str()
            )
        };
        for chunk in chunk_text(&reply, 64) {
            if cancel.is_cancelled() {
                break;
            }
            emit_message(&event_tx, session_id, &chunk);
            tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        }
        push_assistant(self, session_id, &reply);
        Ok(reply)
    }

    /// Run a shell tool with a single live child process. Streams stdout/stderr
    /// to the UI for attach (no second re-exec). Cancel kills the child.
    async fn run_shell_tool(
        &self,
        session_id: Uuid,
        cwd: &Path,
        command: &str,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<()> {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let call_id = Uuid::new_v4().to_string();
        let needs_perm = true;
        let always = {
            let g = self.inner.lock();
            g.always_approve
                || g.always_allowed_tools.contains("run_terminal_cmd")
                || g.permission_mode == "bypassPermissions"
        };

        if needs_perm && !always {
            let req = PermissionRequest {
                id: Uuid::new_v4(),
                session_id,
                tool_name: "run_terminal_cmd".into(),
                summary: format!("Allow tool `run_terminal_cmd`?"),
                detail: serde_json::json!({ "tool": "run_terminal_cmd", "command": command }),
            };
            let (tx, rx) = oneshot::channel();
            {
                let mut g = self.inner.lock();
                g.pending_permissions
                    .insert(req.id, PendingPermission { tx });
            }
            let _ = event_tx.send(SessionUpdate::PermissionRequired {
                session_id,
                request: req,
            });
            let decision = tokio::select! {
                d = rx => d.unwrap_or(PermissionDecision::Deny),
                _ = cancel.cancelled() => PermissionDecision::Deny,
            };
            if decision == PermissionDecision::Deny {
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "run_terminal_cmd".into(),
                    kind: ToolCallKind::Execute,
                    status: ToolCallStatus::Denied,
                    input: serde_json::json!({ "command": command }),
                });
                return Ok(());
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert("run_terminal_cmd".into());
            }
        }

        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: "run_terminal_cmd".into(),
            kind: ToolCallKind::Execute,
            status: ToolCallStatus::Running,
            input: serde_json::json!({ "command": command }),
        });
        // UI attaches to THIS stream — do not re-run the command in another PTY.
        let _ = event_tx.send(SessionUpdate::ShellSessionStarted {
            session_id,
            call_id: call_id.clone(),
            command: command.to_string(),
        });

        let live_shell = self.inner.lock().live_shell.clone();
        let event_tx_chunks = event_tx.clone();
        let call_id_chunks = call_id.clone();
        let result = local_tools::tool_shell_streaming(
            cwd,
            command,
            cancel.clone(),
            live_shell,
            move |chunk| {
                let _ = event_tx_chunks.send(SessionUpdate::ShellOutput {
                    session_id,
                    call_id: call_id_chunks.clone(),
                    data: chunk,
                });
            },
        )
        .await;

        match result {
            Ok(tr) => {
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code: if tr.cancelled { None } else { Some(0) },
                    cancelled: tr.cancelled,
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: if tr.cancelled {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    },
                    output: Some(tr.output),
                });
            }
            Err(e) => {
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code: None,
                    cancelled: cancel.is_cancelled(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(e.to_string()),
                });
            }
        }
        Ok(())
    }

    async fn run_tool_call<F, Fut>(
        &self,
        session_id: Uuid,
        tool_name: &str,
        f: F,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<local_tools::ToolResult>>,
    {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let call_id = Uuid::new_v4().to_string();
        let needs_perm = matches!(tool_name, "run_terminal_cmd" | "write_file");
        let always = {
            let g = self.inner.lock();
            g.always_approve
                || g.always_allowed_tools.contains(tool_name)
                || g.permission_mode == "bypassPermissions"
        };

        if needs_perm && !always {
            let req = PermissionRequest {
                id: Uuid::new_v4(),
                session_id,
                tool_name: tool_name.into(),
                summary: format!("Allow tool `{tool_name}`?"),
                detail: serde_json::json!({ "tool": tool_name }),
            };
            let (tx, rx) = oneshot::channel();
            {
                let mut g = self.inner.lock();
                g.pending_permissions
                    .insert(req.id, PendingPermission { tx });
            }
            let _ = event_tx.send(SessionUpdate::PermissionRequired {
                session_id,
                request: req,
            });
            let decision = tokio::select! {
                d = rx => d.unwrap_or(PermissionDecision::Deny),
                _ = cancel.cancelled() => PermissionDecision::Deny,
            };
            if decision == PermissionDecision::Deny {
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: tool_name.into(),
                    kind: ToolCallKind::Other,
                    status: ToolCallStatus::Denied,
                    input: serde_json::json!({}),
                });
                return Ok(());
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert(tool_name.into());
            }
        }

        let kind = match tool_name {
            "read_file" | "list_dir" => ToolCallKind::Read,
            "write_file" => ToolCallKind::Edit,
            "grep" => ToolCallKind::Search,
            "run_terminal_cmd" => ToolCallKind::Execute,
            _ => ToolCallKind::Other,
        };

        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: tool_name.into(),
            kind,
            status: ToolCallStatus::Running,
            input: serde_json::json!({ "tool": tool_name }),
        });

        match f().await {
            Ok(tr) => {
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Completed,
                    output: Some(tr.output),
                });
            }
            Err(e) => {
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(e.to_string()),
                });
            }
        }
        Ok(())
    }
}

fn push_assistant(host: &AgentHostHandle, session_id: Uuid, text: &str) {
    let mut g = host.inner.lock();
    if let Some(s) = g.sessions.get_mut(&session_id) {
        s.transcript.push(TranscriptEntry {
            role: "assistant".into(),
            text: text.into(),
        });
        s.updated_at = Utc::now();
    }
}

fn emit_message(tx: &mpsc::UnboundedSender<SessionUpdate>, session_id: Uuid, text: &str) {
    let _ = tx.send(SessionUpdate::AgentMessageChunk {
        session_id,
        text: text.into(),
    });
}

fn emit_thought(tx: &mpsc::UnboundedSender<SessionUpdate>, session_id: Uuid, text: &str) {
    let _ = tx.send(SessionUpdate::AgentThoughtChunk {
        session_id,
        text: text.into(),
    });
}

fn chunk_text(s: &str, size: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        cur.push(ch);
        if cur.chars().count() >= size {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn extract_read_path(prompt: &str) -> Option<String> {
    let lower = prompt.to_lowercase();
    for prefix in ["read ", "open ", "cat "] {
        if let Some(i) = lower.find(prefix) {
            let rest = &prompt[i + prefix.len()..];
            let token = rest.split_whitespace().next()?;
            if token.contains('.') || token.contains('/') {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn extract_search_pattern(prompt: &str) -> Option<String> {
    let lower = prompt.to_lowercase();
    for prefix in ["search ", "grep ", "find "] {
        if let Some(i) = lower.find(prefix) {
            let rest = &prompt[i + prefix.len()..];
            let token = rest.split_whitespace().next()?;
            return Some(token.trim_matches('"').to_string());
        }
    }
    None
}

fn extract_shell(prompt: &str) -> Option<String> {
    let lower = prompt.to_lowercase();
    if lower.starts_with("run ") {
        return Some(prompt[4..].trim().to_string());
    }
    if lower.starts_with("shell ") {
        return Some(prompt[6..].trim().to_string());
    }
    if lower.starts_with("$ ") {
        return Some(prompt[2..].trim().to_string());
    }
    None
}

/// `write path/to/file: contents here`
fn extract_write(prompt: &str) -> Option<(String, String)> {
    let lower = prompt.to_lowercase();
    let rest = lower.strip_prefix("write ")?;
    let orig = prompt.get(6..)?;
    if let Some((path, content)) = orig.split_once(':') {
        let path = path.trim().to_string();
        let content = content.trim().to_string();
        if !path.is_empty() {
            return Some((path, content));
        }
    }
    let _ = rest;
    None
}

async fn call_xai_chat(
    creds: &crate::auth_store::WireCredentials,
    model: &str,
    prompt: &str,
    cwd: &Path,
) -> Result<String> {
    // Map desktop aliases to API model ids
    let model_id = match model {
        "grok-build" => "grok-3",
        other => other,
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| anyhow!(e))?;
    let body = serde_json::json!({
        "model": model_id,
        "messages": [
            {
                "role": "system",
                "content": format!(
                    "You are GrokPtah, a desktop coding agent built on Grok Build. \
                     Working directory: {}. Be helpful and concise. Prefer concrete code changes.",
                    cwd.display()
                )
            },
            { "role": "user", "content": prompt }
        ],
        "stream": false
    });
    let base = std::env::var("XAI_API_BASE").unwrap_or_else(|_| "https://api.x.ai/v1".into());
    let mut req = client
        .post(format!("{base}/chat/completions"))
        .bearer_auth(&creds.bearer)
        .header("Content-Type", "application/json");
    // OIDC sessions from `grok login` need the same header Grok Build uses.
    if creds.oidc_token_auth {
        req = req.header("X-XAI-Token-Auth", "true");
    }
    let resp = req
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("request error: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let clipped: String = text.chars().take(800).collect();
        bail!("HTTP {status}: {clipped}");
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| anyhow!("json: {e}"))?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if content.is_empty() {
        bail!("empty model response: {v}");
    }
    Ok(content)
}

