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
use crate::host_helpers::{
    api_context_messages, build_agent_messages, build_compact_summary, call_xai_agent_step,
    call_xai_chat, cargo_test_output_failed, coding_agent_tools, emit_message, emit_thought,
    filter_tools_edit_and_shell, normalize_sandbox_profile, offline_plan_steps, parse_effort_arg,
    propose_plan_with_model, push_assistant, push_thought, push_tool, sandbox_blocks_shell,
    sandbox_is_readonly, surface_rate_limit_or_error, tool_kind, tool_web_fetch, AgentStep,
    McpToolIndex,
};
use crate::local_tools;
use crate::permission::{evaluate_tool_gate, PermissionDecision, PermissionRequest, ToolGate};
use crate::search_engine::{self, SearchHit, SearchQuery};
use crate::session::{Session, SessionKind, SessionSummary, TranscriptEntry};
use crate::session_store::{self, WorkspaceChrome};
use crate::types::{
    AuthState, BackgroundTask, EffortLevel, McpProjectTrust, McpServerInfo, ModelInfo, PluginInfo,
    SkillInfo, SubagentInfo,
};

/// UI restore payload: open tabs + active session + project (sessions live in list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUiState {
    pub project_cwd: Option<String>,
    pub active_session: Option<Uuid>,
    pub open_tab_ids: Vec<Uuid>,
    pub model: String,
    pub effort: EffortLevel,
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub default_model: String,
    pub default_effort: EffortLevel,
    pub always_approve: bool,
    /// Cap model steps per user turn (live eval / tight budgets). None = default 24.
    pub max_agent_rounds: Option<u32>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            // Same source as Grok Build: config.toml [models].default, else
            // preferred id from ~/.grok/models_cache.json, else "grok-build".
            default_model: crate::models_catalog::resolve_default_model(),
            default_effort: EffortLevel::Medium,
            always_approve: false,
            max_agent_rounds: None,
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
    /// Tool that requested this permission (for scoped AlwaysAllow).
    tool_name: String,
    tx: oneshot::Sender<PermissionDecision>,
}

pub(crate) struct Inner {
    running: bool,
    project_cwd: Option<PathBuf>,
    pub(crate) sessions: HashMap<Uuid, Session>,
    active_session: Option<Uuid>,
    /// Tab strip order from the last desktop session (persisted).
    open_tab_ids: Vec<Uuid>,
    always_approve: bool,
    always_allowed_tools: HashSet<String>,
    /// Optional per-turn model-step budget (#187/#188).
    max_agent_rounds: Option<u32>,
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
    /// Per-subagent cancel tokens (#151/#152) — cancel one child without killing siblings.
    subagent_cancels: HashMap<String, CancellationToken>,
    background_tasks: Vec<BackgroundTask>,
    /// Cancel tokens for in-flight background tasks (#52).
    background_cancels: HashMap<String, CancellationToken>,
    pending_permissions: HashMap<Uuid, PendingPermission>,
    /// Per-session turn cancellation so multiple sessions can run concurrently
    /// (Claude Code–style parallel build sessions).
    turn_cancels: HashMap<Uuid, CancellationToken>,
    event_tx: mpsc::UnboundedSender<SessionUpdate>,
    /// Paths the agent wrote/edited this process (for diff review).
    edited_files: Vec<String>,
    /// Per-session path → original content before first agent edit (#146).
    /// Keyed by session so rewind never restores another session's edits.
    edit_snapshots: HashMap<Uuid, HashMap<String, String>>,
    /// Live tool shell child — killed by [`AgentHostHandle::cancel_turn`].
    live_shells: local_tools::LiveShellMap,
    /// Session usage counters (#159) — prompt/completion tokens when API reports them.
    pub(crate) session_usage: HashMap<Uuid, SessionUsage>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    requests: u64,
}

/// Clears `turn_cancels` for a session when dropped — keeps panics from wedging busy.
struct TurnBusyGuard {
    host: AgentHostHandle,
    session_id: Uuid,
    armed: bool,
}

impl Drop for TurnBusyGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut g = self.host.inner.lock();
        g.turn_cancels.remove(&self.session_id);
    }
}

/// Shared handle used by Tauri state and tests.
#[derive(Clone)]
pub struct AgentHostHandle {
    pub(crate) inner: Arc<Mutex<Inner>>,
    event_rx_factory: Arc<Mutex<Option<mpsc::UnboundedReceiver<SessionUpdate>>>>,
    /// Exclusive lock on `~/.grokptah` — kept alive for the process (#119).
    _instance_lock: Option<Arc<crate::instance_lock::InstanceLock>>,
}

pub struct AgentHost;

impl AgentHost {
    /// Create a new host. Events are pulled via [`AgentHostHandle::take_event_receiver`] once.
    pub fn create(config: HostConfig) -> AgentHostHandle {
        // Single-instance guard before any GC or writes that could race another process.
        let instance_lock = match crate::instance_lock::InstanceLock::try_acquire() {
            Ok(l) => Some(Arc::new(l)),
            Err(e) => {
                eprintln!("[grokptah] {e:#}");
                None
            }
        };
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let auth = crate::auth_store::load_auth_state();
        let (chrome, mut sessions) = session_store::load_workspace().unwrap_or_else(|e| {
            eprintln!("[grokptah] workspace load failed: {e:#}");
            (WorkspaceChrome::default(), HashMap::new())
        });
        let project_cwd = session_store::cwd_still_valid(chrome.project_cwd.as_deref());
        let mcp_servers = crate::discover::load_mcp_servers(project_cwd.as_deref());
        let plugins = crate::discover::discover_plugins();
        let skills = crate::discover::discover_skills(project_cwd.as_deref());
        // Prefer persisted model; fall back to HostConfig / catalog default.
        let model = if !chrome.model.is_empty() {
            chrome.model.clone()
        } else {
            config.default_model.clone()
        };
        let mut open_tab_ids = chrome.open_tab_ids.clone();
        // Drop tab ids that no longer exist.
        open_tab_ids.retain(|id| sessions.contains_key(id));
        // Soft GC only when we own the instance lock (never GC another process's sessions).
        if instance_lock.is_some() {
            if let Ok(n) = session_store::garbage_collect(&open_tab_ids, 80, 24 * 7) {
                if n > 0 {
                    if let Ok(reloaded) = session_store::load_all_metas() {
                        sessions = reloaded;
                        open_tab_ids.retain(|id| sessions.contains_key(id));
                    }
                }
            }
        }
        let active_session = chrome
            .active_session
            .filter(|id| sessions.contains_key(id))
            .or_else(|| open_tab_ids.first().copied())
            .or_else(|| sessions.keys().next().copied());
        let inner = Inner {
            running: false,
            project_cwd,
            sessions,
            active_session,
            open_tab_ids,
            always_approve: chrome.always_approve || config.always_approve,
            always_allowed_tools: HashSet::new(),
            max_agent_rounds: config.max_agent_rounds,
            model,
            effort: chrome.effort,
            auth,
            sandbox_profile: if chrome.sandbox_profile.is_empty() {
                "workspace-write".into()
            } else {
                chrome.sandbox_profile
            },
            appearance: if chrome.appearance.is_empty() {
                "dark".into()
            } else {
                chrome.appearance
            },
            permission_mode: "default".into(),
            allow_rules: Vec::new(),
            deny_rules: Vec::new(),
            mcp_servers,
            plugins,
            skills,
            subagents: Vec::new(),
            subagent_cancels: HashMap::new(),
            background_tasks: Vec::new(),
            background_cancels: HashMap::new(),
            pending_permissions: HashMap::new(),
            turn_cancels: HashMap::new(),
            event_tx,
            edited_files: Vec::new(),
            edit_snapshots: HashMap::new(), // session_id → path → original
            live_shells: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            session_usage: HashMap::new(),
        };
        AgentHostHandle {
            inner: Arc::new(Mutex::new(inner)),
            event_rx_factory: Arc::new(Mutex::new(Some(event_rx))),
            _instance_lock: instance_lock,
        }
    }
}

impl AgentHostHandle {
    pub fn take_event_receiver(&self) -> Option<mpsc::UnboundedReceiver<SessionUpdate>> {
        self.event_rx_factory.lock().take()
    }

    /// Persist tiny workspace chrome (tabs / project / model) only.
    pub fn persist_chrome(&self) {
        let chrome = {
            let g = self.inner.lock();
            WorkspaceChrome {
                version: 2,
                project_cwd: g.project_cwd.as_ref().map(|p| p.display().to_string()),
                active_session: g.active_session,
                open_tab_ids: g.open_tab_ids.clone(),
                model: g.model.clone(),
                effort: g.effort,
                sandbox_profile: g.sandbox_profile.clone(),
                appearance: g.appearance.clone(),
                always_approve: g.always_approve,
            }
        };
        if let Err(e) = session_store::save_chrome(&chrome) {
            eprintln!("[grokptah] chrome persist failed: {e:#}");
        }
    }

    /// Append new transcript lines + refresh meta for one session.
    pub fn persist_session(&self, id: Uuid) {
        let mut session = {
            let g = self.inner.lock();
            match g.sessions.get(&id) {
                Some(s) => s.clone(),
                None => return,
            }
        };
        // Ensure we only append what isn't on disk yet.
        if !session.transcript_loaded {
            // Still push meta (title/count) without loading body.
            if let Err(e) = session_store::save_session_meta(&session) {
                eprintln!("[grokptah] meta persist failed: {e:#}");
            }
            return;
        }
        let from = session.persisted_len;
        match session_store::append_transcript(&session, from) {
            Ok(n) => {
                session.persisted_len += n;
                let mut g = self.inner.lock();
                if let Some(s) = g.sessions.get_mut(&id) {
                    s.persisted_len = session.persisted_len;
                }
            }
            Err(e) => eprintln!("[grokptah] transcript append failed: {e:#}"),
        }
        // Always refresh meta (compact cursor, title, counts) even when no new lines.
        if let Err(e) = session_store::save_session_meta(&session) {
            eprintln!("[grokptah] meta persist failed: {e:#}");
        }
    }

    /// Full transcript rewrite (rewind / fork only — never used by compact).
    pub fn persist_session_rewrite(&self, id: Uuid) {
        let session = {
            let g = self.inner.lock();
            match g.sessions.get(&id) {
                Some(s) => s.clone(),
                None => return,
            }
        };
        if let Err(e) = session_store::rewrite_transcript(&session) {
            eprintln!("[grokptah] transcript rewrite failed: {e:#}");
            return;
        }
        let mut g = self.inner.lock();
        if let Some(s) = g.sessions.get_mut(&id) {
            s.persisted_len = s.transcript.len();
            s.transcript_loaded = true;
        }
    }

    /// Back-compat alias used by older call sites — chrome only.
    pub fn persist(&self) {
        self.persist_chrome();
    }

    /// UI restore: sessions list + which tabs were open.
    pub fn workspace_ui_state(&self) -> WorkspaceUiState {
        let g = self.inner.lock();
        // Active (non-archived) only for default restore list.
        let mut sessions: Vec<_> = g
            .sessions
            .values()
            .filter(|s| !s.archived)
            .map(|s| s.summary())
            .collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        WorkspaceUiState {
            project_cwd: g.project_cwd.as_ref().map(|p| p.display().to_string()),
            active_session: g.active_session,
            open_tab_ids: g.open_tab_ids.clone(),
            model: g.model.clone(),
            effort: g.effort,
            sessions,
        }
    }

    /// Remember open tabs (call when the tab strip changes).
    pub fn set_open_tabs(&self, ids: Vec<Uuid>, active: Option<Uuid>) {
        {
            let mut g = self.inner.lock();
            g.open_tab_ids = ids
                .into_iter()
                .filter(|id| g.sessions.contains_key(id))
                .collect();
            if let Some(a) = active {
                if g.sessions.contains_key(&a) {
                    g.active_session = Some(a);
                }
            }
        }
        self.persist_chrome();
    }

    /// Ensure transcript is in memory (lazy load from JSONL).
    pub fn ensure_transcript_loaded(&self, id: Uuid) -> Result<()> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        if s.transcript_loaded {
            return Ok(());
        }
        session_store::load_transcript(s)?;
        Ok(())
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
        if self._instance_lock.is_none() {
            bail!(
                "another GrokPtah instance is already using {}. \
                 Quit the other window before starting a second one.",
                crate::discover::grokptah_home().display()
            );
        }
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
        {
            let mut g = self.inner.lock();
            g.project_cwd = Some(p.clone());
            g.mcp_servers = mcp;
            g.skills = skills;
        }
        self.persist_chrome();
        Ok(p.display().to_string())
    }

    pub fn session_new(&self) -> Result<SessionSummary> {
        self.session_new_kind(SessionKind::Build)
    }

    pub fn session_new_kind(&self, kind: SessionKind) -> Result<SessionSummary> {
        let summary = {
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
            let s = Session::new_with_kind(cwd, model, effort, kind);
            let summary = s.summary();
            g.active_session = Some(s.id);
            if !g.open_tab_ids.contains(&s.id) {
                g.open_tab_ids.push(s.id);
            }
            g.sessions.insert(s.id, s);
            summary
        };
        // Empty shell: meta + empty transcript file.
        self.persist_session_rewrite(summary.id);
        self.persist_chrome();
        Ok(summary)
    }

    /// Hybrid / keyword / semantic search over chats + builds.
    pub fn search_sessions(&self, query: SearchQuery) -> Result<Vec<SearchHit>> {
        search_engine::search(&query).map_err(|e| anyhow!(e))
    }

    pub fn session_load(&self, id: Uuid) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        // Build sessions pin their own project root — promote it so files/git
        // panels track the session you just opened.
        let (kind, cwd) = {
            let g = self.inner.lock();
            let s = g
                .sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            (s.kind, s.cwd.clone())
        };
        // #167: rebind cwd when the stored path moved (project open or host project).
        if kind == SessionKind::Build {
            if !cwd.is_dir() {
                let rebind = {
                    let g = self.inner.lock();
                    g.project_cwd
                        .clone()
                        .filter(|p| p.is_dir())
                        .or_else(|| std::env::current_dir().ok().filter(|p| p.is_dir()))
                };
                if let Some(new_cwd) = rebind {
                    let mut g = self.inner.lock();
                    if let Some(s) = g.sessions.get_mut(&id) {
                        s.cwd = new_cwd.clone();
                        s.updated_at = Utc::now();
                    }
                    drop(g);
                    self.persist_session_meta_only(id);
                    let _ = self.set_project_cwd(&new_cwd);
                }
            } else {
                let current = self.inner.lock().project_cwd.clone();
                if current.as_ref() != Some(&cwd) {
                    let _ = self.set_project_cwd(&cwd);
                }
            }
        }
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let summary = s.summary();
            g.active_session = Some(id);
            if !g.open_tab_ids.contains(&id) {
                g.open_tab_ids.push(id);
            }
            summary
        };
        // #152: restore historical subagent summary when reopening a session.
        self.load_session_subagents(id);
        self.persist_chrome();
        Ok(summary)
    }

    /// Full transcript for hydrating a session tab (loads JSONL on demand).
    pub fn session_transcript(&self, id: Uuid) -> Result<Vec<TranscriptEntry>> {
        self.ensure_transcript_loaded(id)?;
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
        self.list_sessions_filtered(false)
    }

    /// When `archived_only` is true, return only archived; otherwise only active.
    pub fn list_sessions_filtered(&self, archived_only: bool) -> Vec<SessionSummary> {
        self.list_sessions_ex(archived_only, None)
    }

    /// Optional kind filter: Some(Chat) / Some(Build) / None = all kinds.
    pub fn list_sessions_ex(
        &self,
        archived_only: bool,
        kind: Option<SessionKind>,
    ) -> Vec<SessionSummary> {
        let g = self.inner.lock();
        let mut v: Vec<_> = g
            .sessions
            .values()
            .filter(|s| s.archived == archived_only)
            .filter(|s| kind.map(|k| s.kind == k).unwrap_or(true))
            .map(|s| s.summary())
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn list_all_sessions(&self) -> Vec<SessionSummary> {
        let g = self.inner.lock();
        let mut v: Vec<_> = g.sessions.values().map(|s| s.summary()).collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn list_sessions_by_kind(
        &self,
        kind: SessionKind,
        include_archived: bool,
    ) -> Vec<SessionSummary> {
        let g = self.inner.lock();
        let mut v: Vec<_> = g
            .sessions
            .values()
            .filter(|s| s.kind == kind)
            .filter(|s| include_archived || !s.archived)
            .map(|s| s.summary())
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn session_rename(&self, id: Uuid, title: String) -> Result<SessionSummary> {
        let title = title.trim().to_string();
        if title.is_empty() {
            bail!("title must not be empty");
        }
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.title = title;
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id);
        Ok(summary)
    }

    pub fn session_delete(&self, id: Uuid) -> Result<()> {
        {
            let mut g = self.inner.lock();
            if !g.sessions.contains_key(&id) {
                bail!("unknown session");
            }
            if g.turn_cancels.contains_key(&id) {
                bail!("cannot delete a session with an active turn — stop it first");
            }
            g.sessions.remove(&id);
            g.open_tab_ids.retain(|t| *t != id);
            if g.active_session == Some(id) {
                g.active_session = g.open_tab_ids.first().copied();
            }
        }
        session_store::delete_session(id)?;
        self.persist_chrome();
        Ok(())
    }

    pub fn session_archive(&self, id: Uuid, archived: bool) -> Result<SessionSummary> {
        let summary = {
            let mut g = self.inner.lock();
            {
                let s = g
                    .sessions
                    .get_mut(&id)
                    .ok_or_else(|| anyhow!("unknown session"))?;
                s.archived = archived;
                s.archived_at = if archived { Some(Utc::now()) } else { None };
                s.updated_at = Utc::now();
            }
            // Closing archived sessions out of the tab strip
            if archived {
                g.open_tab_ids.retain(|t| *t != id);
                if g.active_session == Some(id) {
                    g.active_session = g.open_tab_ids.first().copied();
                }
            }
            g.sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?
                .summary()
        };
        self.persist_session_meta_only(id);
        self.persist_chrome();
        Ok(summary)
    }

    pub fn session_set_folder(&self, id: Uuid, folder: Option<String>) -> Result<SessionSummary> {
        let folder = folder.and_then(|f| {
            let t = f.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        });
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.folder = folder;
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id);
        Ok(summary)
    }

    /// Set the working directory for a session (tools + shell run here).
    ///
    /// For build sessions this is the project root. When the session is active,
    /// also updates the host project cwd so the files/git panels match.
    pub fn session_set_cwd(&self, id: Uuid, path: impl AsRef<Path>) -> Result<SessionSummary> {
        let p = path.as_ref().to_path_buf();
        if !p.is_dir() {
            bail!("not a directory: {}", p.display());
        }
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.cwd = p.clone();
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id);

        // Keep host workspace + discovery in sync when this is the focused session
        // or when no project is open yet.
        let should_sync = {
            let g = self.inner.lock();
            g.active_session == Some(id) || g.project_cwd.is_none()
        };
        if should_sync {
            let _ = self.set_project_cwd(&p);
        }
        Ok(summary)
    }

    pub fn session_set_tags(&self, id: Uuid, tags: Vec<String>) -> Result<SessionSummary> {
        let mut clean: Vec<String> = tags
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        clean.sort();
        clean.dedup();
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            s.tags = clean;
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_meta_only(id);
        Ok(summary)
    }

    /// Unique folder names from non-archived sessions (plus any archived if requested).
    pub fn list_folders(&self, include_archived: bool) -> Vec<String> {
        let g = self.inner.lock();
        let mut set = std::collections::BTreeSet::new();
        for s in g.sessions.values() {
            if !include_archived && s.archived {
                continue;
            }
            if let Some(f) = &s.folder {
                if !f.is_empty() {
                    set.insert(f.clone());
                }
            }
        }
        set.into_iter().collect()
    }

    pub fn list_tags(&self, include_archived: bool) -> Vec<String> {
        let g = self.inner.lock();
        let mut set = std::collections::BTreeSet::new();
        for s in g.sessions.values() {
            if !include_archived && s.archived {
                continue;
            }
            for t in &s.tags {
                if !t.is_empty() {
                    set.insert(t.clone());
                }
            }
        }
        set.into_iter().collect()
    }

    fn persist_session_meta_only(&self, id: Uuid) {
        let session = {
            let g = self.inner.lock();
            match g.sessions.get(&id) {
                Some(s) => s.clone(),
                None => return,
            }
        };
        if let Err(e) = session_store::save_session_meta(&session) {
            eprintln!("[grokptah] meta persist failed: {e:#}");
        }
    }

    pub fn fork_session(&self, source: Uuid) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(source)?;
        let summary = {
            let mut g = self.inner.lock();
            let src = g
                .sessions
                .get(&source)
                .ok_or_else(|| anyhow!("unknown session"))?
                .clone();
            let mut s = Session::new(src.cwd.clone(), src.model.clone(), src.effort);
            s.transcript = src.transcript.clone();
            s.transcript_loaded = true;
            s.persisted_len = 0;
            s.title = format!("{} (fork)", src.title);
            s.forked_from = Some(source);
            s.plan_mode = src.plan_mode;
            s.plan_steps = src.plan_steps.clone();
            s.plan_status = src.plan_status.clone();
            s.plan_goal = src.plan_goal.clone();
            s.compacted_summary = src.compacted_summary.clone();
            s.api_context_start = src.api_context_start;
            s.kind = src.kind;
            let summary = s.summary();
            g.active_session = Some(s.id);
            if !g.open_tab_ids.contains(&s.id) {
                g.open_tab_ids.push(s.id);
            }
            g.sessions.insert(s.id, s);
            summary
        };
        // Forked body is a full new log.
        self.persist_session_rewrite(summary.id);
        self.persist_chrome();
        Ok(summary)
    }

    /// Rewind conversation and/or restore files (#146).
    ///
    /// `mode`: `conversation` | `files` | `all` (default `conversation`).
    pub fn rewind_session(
        &self,
        id: Uuid,
        keep_messages: usize,
        mode: &str,
    ) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        let mode = mode.trim().to_ascii_lowercase();
        let do_files = mode == "files" || mode == "all" || mode == "filesonly";
        let do_conv = mode != "files" && mode != "filesonly";

        if do_files {
            self.restore_edit_snapshots_for_session(id)?;
        }

        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            // Honor keep_messages for conversation modes (#146).
            // FilesOnly leaves transcript untouched.
            if do_conv && keep_messages < s.transcript.len() {
                s.transcript.truncate(keep_messages);
            }
            s.updated_at = Utc::now();
            s.summary()
        };
        if do_conv {
            self.persist_session_rewrite(id);
        }
        Ok(summary)
    }

    /// Snapshot original contents the first time a path is edited in this session (#146).
    pub fn snapshot_edit_original(&self, cwd: &Path, rel_path: &str) {
        let abs = cwd.join(rel_path);
        let key = abs.to_string_lossy().into_owned();
        let mut g = self.inner.lock();
        let sid = match g.active_session {
            Some(id) => id,
            None => return,
        };
        let map = g.edit_snapshots.entry(sid).or_default();
        if map.contains_key(&key) {
            return;
        }
        let original = std::fs::read_to_string(&abs).unwrap_or_default();
        map.insert(key, original);
    }

    /// Snapshot for an explicit session (tool path always knows the session).
    pub fn snapshot_edit_original_for_session(&self, session_id: Uuid, cwd: &Path, rel_path: &str) {
        let abs = cwd.join(rel_path);
        let key = abs.to_string_lossy().into_owned();
        let mut g = self.inner.lock();
        let map = g.edit_snapshots.entry(session_id).or_default();
        if map.contains_key(&key) {
            return;
        }
        let original = std::fs::read_to_string(&abs).unwrap_or_default();
        map.insert(key, original);
    }

    fn restore_edit_snapshots_for_session(&self, session_id: Uuid) -> Result<()> {
        let snaps: Vec<(String, String)> = {
            let g = self.inner.lock();
            g.edit_snapshots
                .get(&session_id)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
        for (path, content) in &snaps {
            let p = Path::new(path);
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if content.is_empty() && !p.exists() {
                continue;
            }
            if content.is_empty() {
                let _ = std::fs::remove_file(p);
            } else {
                std::fs::write(p, content)?;
            }
        }
        let mut g = self.inner.lock();
        g.edit_snapshots.remove(&session_id);
        // Drop edited_files entries that match restored paths (best-effort)
        let restored: std::collections::HashSet<_> = snaps.iter().map(|(p, _)| p.clone()).collect();
        g.edited_files
            .retain(|p| !restored.iter().any(|r| r.ends_with(p) || p.ends_with(r)));
        Ok(())
    }

    /// Shrink the *server-facing* context window only (sync extractive path).
    ///
    /// Local `transcript.jsonl` is never truncated or rewritten: every message
    /// stays on disk for search, UI, and perpetual history. Compact advances
    /// [`Session::api_context_start`] and stores a summary of the portion that
    /// leaves the API window in [`Session::compacted_summary`].
    pub fn compact_session(&self, id: Uuid) -> Result<SessionSummary> {
        self.compact_session_inner(id, None)
    }

    /// Compact with optional LLM-quality summary text for the leaving window.
    pub fn compact_session_with_summary(
        &self,
        id: Uuid,
        quality_summary: Option<String>,
    ) -> Result<SessionSummary> {
        self.compact_session_inner(id, quality_summary)
    }

    fn compact_session_inner(
        &self,
        id: Uuid,
        quality_summary: Option<String>,
    ) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        const KEEP_RECENT: usize = 6;
        let (summary, cwd, leaving_for_memory) = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let len_before = s.transcript.len();
            let mut leaving_texts = Vec::new();
            if len_before > KEEP_RECENT {
                let new_start = len_before - KEEP_RECENT;
                let old_start = s.api_context_start.min(len_before);
                if new_start > old_start {
                    let leaving = &s.transcript[old_start..new_start];
                    leaving_texts = leaving
                        .iter()
                        .filter(|e| e.role == "user" || e.role == "assistant")
                        .map(|e| e.text.clone())
                        .collect();
                    let piece = quality_summary
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| build_compact_summary(leaving));
                    s.compacted_summary = Some(match s.compacted_summary.take() {
                        Some(prev) if !prev.is_empty() => format!("{prev}\n\n{piece}"),
                        _ => piece,
                    });
                    s.api_context_start = new_start;
                    let total = s.transcript.len();
                    let in_window = total - new_start;
                    // Additive local notice only — never deletes prior entries.
                    s.transcript.push(TranscriptEntry::system(format!(
                        "[context compacted for server: {in_window} recent messages stay in the API window; full local history retained ({total} messages before this notice)]"
                    )));
                    debug_assert!(s.transcript.len() >= len_before);
                }
            }
            s.updated_at = Utc::now();
            (s.summary(), s.cwd.clone(), leaving_texts)
        };
        // Best-effort memory flush of key decisions from compacted window.
        for t in leaving_for_memory.iter().take(12) {
            let lower = t.to_ascii_lowercase();
            if lower.contains("always ")
                || lower.contains("decision:")
                || lower.contains("remember:")
                || lower.contains("prefer ")
            {
                let clip: String = t.chars().take(400).collect();
                let _ = crate::memory::remember(&cwd, &clip, &["compact-flush".into()]);
            }
        }
        self.persist_session(id);
        Ok(summary)
    }

    /// Async compact: model-backed summary when online, extractive offline.
    pub async fn compact_session_async(&self, id: Uuid) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        const KEEP_RECENT: usize = 6;
        let (cwd, leaving, model) = {
            let g = self.inner.lock();
            let s = g
                .sessions
                .get(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let len = s.transcript.len();
            if len <= KEEP_RECENT {
                drop(g);
                return self.compact_session(id);
            }
            let new_start = len - KEEP_RECENT;
            let old_start = s.api_context_start.min(len);
            if new_start <= old_start {
                drop(g);
                return self.compact_session(id);
            }
            let leaving = s.transcript[old_start..new_start].to_vec();
            (s.cwd.clone(), leaving, g.model.clone())
        };

        let quality = if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
            None
        } else if let Some(creds) = crate::auth_store::resolve_wire_credentials() {
            let blob = build_compact_summary(&leaving);
            let prompt = format!(
                "Summarize this coding-agent conversation for future turns. \
                 Preserve: user goals, decisions, file paths touched, failing tests, open TODOs. \
                 Be dense (≤600 words). Do not invent facts.\n\n{blob}"
            );
            match call_xai_chat(
                &creds,
                &model,
                &[("user".into(), prompt)],
                None,
                &cwd,
                SessionKind::Build,
            )
            .await
            {
                Ok(t) if !t.trim().is_empty() => Some(format!("LLM compact summary:\n{t}")),
                _ => None,
            }
        } else {
            None
        };

        self.compact_session_with_summary(id, quality)
    }

    /// Export full local transcript (never truncated by compact) as text.
    pub fn export_transcript(&self, id: Uuid) -> Result<String> {
        self.ensure_transcript_loaded(id)?;
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        let mut out = format!(
            "# GrokPtah transcript export\n\
             session: {}\n\
             title: {}\n\
             cwd: {}\n\
             model: {}\n\
             messages: {}\n\
             api_context_start: {}\n\
             compacted_summary_chars: {}\n\n",
            s.id,
            s.title,
            s.cwd.display(),
            s.model,
            s.transcript.len(),
            s.api_context_start,
            s.compacted_summary.as_ref().map(|c| c.len()).unwrap_or(0)
        );
        for (i, e) in s.transcript.iter().enumerate() {
            out.push_str(&format!("## [{i}] {}\n{}\n\n", e.role, e.text));
        }
        Ok(out)
    }

    /// Compact metadata for tests / diagnostics (local length never shrinks on compact).
    pub fn compact_stats(&self, id: Uuid) -> Result<(usize, usize, Option<String>)> {
        self.ensure_transcript_loaded(id)?;
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        Ok((
            s.transcript.len(),
            s.api_context_start,
            s.compacted_summary.clone(),
        ))
    }

    /// Build the same OpenAI message list the coding agent would send (system +
    /// compacted summary + windowed history). Used by tests and diagnostics so
    /// offline paths can still assert wire context quality after `/compact`.
    pub fn wire_messages_preview(&self, id: Uuid) -> Result<Vec<serde_json::Value>> {
        self.ensure_transcript_loaded(id)?;
        let g = self.inner.lock();
        let s = g
            .sessions
            .get(&id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        let history = api_context_messages(s);
        let plan = if matches!(s.plan_status.as_str(), "accepted" | "executing" | "done")
            && !s.plan_steps.is_empty()
        {
            Some((
                s.plan_goal.as_deref().unwrap_or("execute plan"),
                s.plan_steps.as_slice(),
            ))
        } else {
            None
        };
        Ok(build_agent_messages(
            &history,
            s.compacted_summary.as_deref(),
            &s.cwd,
            plan,
        ))
    }

    /// Test-only: mark the session busy then panic. [`TurnBusyGuard`] must clear
    /// busy on unwind so a follow-up turn is accepted.
    ///
    /// `#[doc(hidden)]` — for integration tests (not a product API).
    #[doc(hidden)]
    pub fn test_only_panic_while_turn_busy(&self, session_id: Uuid) {
        let cancel = CancellationToken::new();
        {
            let mut g = self.inner.lock();
            g.turn_cancels.insert(session_id, cancel);
        }
        assert!(self.session_busy(session_id));
        let _guard = TurnBusyGuard {
            host: self.clone(),
            session_id,
            armed: true,
        };
        panic!("simulated mid-turn panic");
    }

    /// Surface a rate-limit (or other) agent failure the same way a live turn
    /// does — emits [`SessionUpdate::RateLimited`] when appropriate plus a
    /// user-visible error chunk. Public for offline resilience tests.
    pub fn surface_agent_failure(&self, session_id: Uuid, err: &str) -> Result<()> {
        let event_tx = self.inner.lock().event_tx.clone();
        surface_rate_limit_or_error(&event_tx, session_id, err);
        Ok(())
    }

    /// Last agent-edited path for this process (for one-click diff UI).
    pub fn last_edited_path(&self) -> Option<String> {
        self.inner.lock().edited_files.last().cloned()
    }

    pub fn memory_list(&self) -> Result<Vec<crate::memory::MemoryFact>> {
        let cwd = self
            .inner
            .lock()
            .project_cwd
            .clone()
            .ok_or_else(|| anyhow!("no project open"))?;
        Ok(crate::memory::list_facts(&cwd))
    }

    pub fn memory_remember(&self, text: &str) -> Result<String> {
        let cwd = self
            .inner
            .lock()
            .project_cwd
            .clone()
            .ok_or_else(|| anyhow!("no project open"))?;
        crate::memory::remember(&cwd, text, &[]).map_err(|e| anyhow!(e))
    }

    pub fn set_model(&self, model: String) {
        self.inner.lock().model = model;
        self.persist_chrome();
    }

    /// Accumulate token usage for /usage (#159).
    pub fn record_session_usage(
        &self,
        session_id: Uuid,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    ) {
        let mut g = self.inner.lock();
        let u = g.session_usage.entry(session_id).or_default();
        u.prompt_tokens = u.prompt_tokens.saturating_add(prompt_tokens);
        u.completion_tokens = u.completion_tokens.saturating_add(completion_tokens);
        u.total_tokens = u.total_tokens.saturating_add(if total_tokens > 0 {
            total_tokens
        } else {
            prompt_tokens.saturating_add(completion_tokens)
        });
        u.requests = u.requests.saturating_add(1);
    }

    pub fn set_effort(&self, effort: EffortLevel) {
        self.inner.lock().effort = effort;
        self.persist_chrome();
    }

    /// Single source of truth for global tool prompting (#113).
    ///
    /// `true`  → always_approve + permission_mode=bypassPermissions  
    /// `false` → prompt mode
    pub fn set_always_approve(&self, v: bool) {
        let mut g = self.inner.lock();
        g.always_approve = v;
        g.permission_mode = if v {
            "bypassPermissions".into()
        } else {
            "default".into()
        };
        drop(g);
        self.persist_chrome();
    }

    pub fn set_sandbox(&self, profile: String) {
        self.inner.lock().sandbox_profile = normalize_sandbox_profile(&profile).to_string();
        self.persist_chrome();
    }

    pub fn set_appearance(&self, appearance: String) {
        self.inner.lock().appearance = appearance;
        self.persist_chrome();
    }

    /// Keep permission_mode and always_approve as one coherent control (#113).
    pub fn set_permission_mode(&self, mode: String) {
        let mut g = self.inner.lock();
        let bypass = mode == "bypassPermissions" || mode == "bypass" || mode == "yolo";
        g.permission_mode = if bypass {
            "bypassPermissions".into()
        } else {
            "default".into()
        };
        g.always_approve = bypass;
        drop(g);
        self.persist_chrome();
    }

    pub fn set_allow_deny_rules(&self, allow: Vec<String>, deny: Vec<String>) {
        let mut g = self.inner.lock();
        g.allow_rules = allow;
        g.deny_rules = deny;
    }

    /// Consult YOLO / always-allowed tools / allow+deny rules for a tool gate.
    fn tool_gate(&self, tool_name: &str) -> ToolGate {
        let g = self.inner.lock();
        evaluate_tool_gate(
            tool_name,
            g.always_approve,
            &g.always_allowed_tools,
            &g.permission_mode,
            &g.allow_rules,
            &g.deny_rules,
        )
    }

    pub fn models(&self) -> Vec<ModelInfo> {
        // Live catalog from Grok Build's models_cache.json + builtins (grok-build, …).
        crate::models_catalog::load_catalog()
            .into_iter()
            .map(|m| m.info)
            .collect()
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
        let state =
            crate::auth_store::store_api_key(&api_key, &display_name).map_err(|e| anyhow!(e))?;
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

    /// Project-local MCP trust status for the open project (or defaults).
    pub fn mcp_project_trust(&self) -> McpProjectTrust {
        let project = self.inner.lock().project_cwd.clone();
        match project {
            Some(p) => McpProjectTrust {
                project: Some(p.display().to_string()),
                has_local_mcp: crate::discover::project_has_local_mcp_servers(&p),
                trusted: crate::discover::is_project_mcp_trusted(&p),
                decided: crate::discover::project_mcp_trust_decided(&p),
            },
            None => McpProjectTrust {
                project: None,
                has_local_mcp: false,
                trusted: false,
                decided: false,
            },
        }
    }

    pub fn mcp_set_project_trust(&self, trusted: bool) -> Result<McpProjectTrust> {
        let project = self
            .inner
            .lock()
            .project_cwd
            .clone()
            .ok_or_else(|| anyhow!("no project open"))?;
        crate::discover::set_project_mcp_trusted(&project, trusted).map_err(|e| anyhow!(e))?;
        Ok(self.mcp_project_trust())
    }

    pub fn mcp_set_enabled(&self, name: &str, enabled: bool) -> Result<McpServerInfo> {
        let project = self.inner.lock().project_cwd.clone();
        if !crate::discover::save_mcp_server_enabled(project.as_deref(), name, enabled) {
            // still update in-memory for tests without config file write success
            let mut g = self.inner.lock();
            if let Some(s) = g.mcp_servers.iter_mut().find(|s| s.name == name) {
                s.enabled = enabled;
                s.status = if enabled {
                    "configured".into()
                } else {
                    "disabled".into()
                };
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

    /// #164 agent definitions from `.grok/agents` / `.grokptah/agents`.
    pub fn list_agents(&self) -> Vec<crate::agents_personas::AgentDef> {
        let project = self.inner.lock().project_cwd.clone();
        crate::agents_personas::discover_agents(project.as_deref())
    }

    /// #164 personas from `.grok/personas` / `.grokptah/personas`.
    pub fn list_personas(&self) -> Vec<crate::agents_personas::PersonaDef> {
        let project = self.inner.lock().project_cwd.clone();
        crate::agents_personas::discover_personas(project.as_deref())
    }

    /// #165 accurate count of running subagents for a session (or all).
    pub fn running_subagent_count(&self, session_id: Option<Uuid>) -> usize {
        let g = self.inner.lock();
        g.subagents
            .iter()
            .filter(|s| s.status == "running")
            .filter(|s| match session_id {
                None => true,
                Some(want) => {
                    s.session_id
                        .as_ref()
                        .and_then(|sid| Uuid::parse_str(sid).ok())
                        == Some(want)
                }
            })
            .count()
    }

    /// #174 fleet observability snapshot (usage + running subagents per session).
    pub fn fleet_observability(&self) -> serde_json::Value {
        let g = self.inner.lock();
        let mut sessions = Vec::new();
        for (id, s) in &g.sessions {
            let running = g
                .subagents
                .iter()
                .filter(|a| {
                    a.status == "running"
                        && a.session_id.as_deref() == Some(id.to_string().as_str())
                })
                .count();
            let usage = g.session_usage.get(id).cloned().unwrap_or_default();
            sessions.push(serde_json::json!({
                "session_id": id.to_string(),
                "title": s.title,
                "busy": g.turn_cancels.contains_key(id),
                "running_subagents": running,
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
                "usage_requests": usage.requests,
            }));
        }
        let running_total = g.subagents.iter().filter(|s| s.status == "running").count();
        serde_json::json!({
            "running_subagents_total": running_total,
            "sessions": sessions,
        })
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

    /// Public spawn entry for tools, Tauri, and tests (#151).
    ///
    /// Starts the child on a background task and returns immediately so multiple
    /// children overlap. When a parent turn is active, child cancel is linked to
    /// that turn's token.
    pub async fn spawn_subagent_public(
        &self,
        session_id: Uuid,
        kind: &str,
        prompt: &str,
    ) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.sessions
                .get(&session_id)
                .map(|s| s.cwd.clone())
                .or_else(|| g.project_cwd.clone())
                .ok_or_else(|| anyhow!("no project/session cwd"))?
        };
        let parent_cancel = {
            let g = self.inner.lock();
            g.turn_cancels
                .get(&session_id)
                .cloned()
                .unwrap_or_else(CancellationToken::new)
        };
        let event_tx = self.inner.lock().event_tx.clone();
        self.spawn_gp_subagent_parallel(session_id, &cwd, prompt, kind, &parent_cancel, &event_tx)
    }

    /// Test helper: register a parent turn cancel token for `session_id`.
    pub fn begin_turn_for_test(&self, session_id: Uuid) {
        let mut g = self.inner.lock();
        g.turn_cancels.entry(session_id).or_default();
    }

    /// Cancel a single subagent without cancelling the parent turn or siblings (#152).
    pub fn cancel_subagent(&self, id: &str) -> Result<()> {
        let mut g = self.inner.lock();
        if let Some(token) = g.subagent_cancels.remove(id) {
            token.cancel();
        } else {
            // Still mark status if present.
        }
        let s = g
            .subagents
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow!("unknown subagent {id}"))?;
        if s.status == "running" {
            s.status = "cancelled".into();
            s.summary = Some("cancelled by user".into());
        }
        let session_id = s.session_id.as_ref().and_then(|x| Uuid::parse_str(x).ok());
        let snap = g.subagents.clone();
        drop(g);
        if let Some(sid) = session_id {
            let _ = session_store::save_session_subagents(sid, &snap);
            let tx = self.inner.lock().event_tx.clone();
            let _ = tx.send(SessionUpdate::SubagentUpdate {
                session_id: sid,
                subagent_id: id.to_string(),
                status: "cancelled".into(),
                detail: Some("cancelled by user".into()),
            });
        }
        Ok(())
    }

    /// Load durable subagent history for a session into the live list (#152).
    pub fn load_session_subagents(&self, session_id: Uuid) {
        let hist = session_store::load_session_subagents(session_id);
        if hist.is_empty() {
            return;
        }
        let mut g = self.inner.lock();
        // Drop stale rows for this session; keep other sessions' live rows.
        g.subagents
            .retain(|s| s.session_id.as_deref() != Some(&session_id.to_string()));
        g.subagents.extend(hist);
    }

    pub fn background_tasks(&self) -> Vec<BackgroundTask> {
        self.inner.lock().background_tasks.clone()
    }

    pub fn cancel_background_task(&self, id: &str) -> Result<()> {
        // Agent tool shells: cancel the owning turn (kills live_shells) (#52).
        let session_for_shell = {
            let g = self.inner.lock();
            g.background_tasks
                .iter()
                .find(|t| t.id == id)
                .and_then(|t| t.session_id.clone())
        };
        if id.starts_with("shell-") {
            if let Some(sid) = session_for_shell.as_deref() {
                if let Ok(uuid) = Uuid::parse_str(sid) {
                    let _ = self.cancel_turn(Some(uuid));
                }
            }
        }
        let mut g = self.inner.lock();
        if let Some(token) = g.background_cancels.remove(id) {
            token.cancel();
        }
        let t = g
            .background_tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("unknown task"))?;
        t.status = "cancelled".into();
        t.detail = Some("cancelled by user".into());
        Ok(())
    }

    /// Schedule long-running work visible outside the transcript (#52).
    ///
    /// - Title starting with `!` runs a shell command in the project cwd.
    /// - Otherwise runs a cancellable project file scan with progress.
    pub fn schedule_background_task(&self, title: String) -> BackgroundTask {
        let id = Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let is_shell = title.trim_start().starts_with('!');
        let t = BackgroundTask {
            id: id.clone(),
            title: title.clone(),
            status: "running".into(),
            scheduled: true,
            kind: if is_shell {
                "shell".into()
            } else {
                "scan".into()
            },
            session_id: self.inner.lock().active_session.map(|u| u.to_string()),
            detail: Some("starting…".into()),
        };
        {
            let mut g = self.inner.lock();
            g.background_tasks.push(t.clone());
            g.background_cancels.insert(id.clone(), cancel.clone());
        }
        let host = self.clone();
        let task_id = id.clone();
        let event_tx = self.inner.lock().event_tx.clone();
        let title_for_task = title.clone();
        tokio::spawn(async move {
            let final_status = if is_shell {
                let cmd = title_for_task.trim_start().trim_start_matches('!').trim();
                let cwd = host.inner.lock().project_cwd.clone();
                let cancel_c = cancel.clone();
                let cmd_owned = cmd.to_string();
                let run = async {
                    let mut command = tokio::process::Command::new("sh");
                    command.arg("-lc").arg(&cmd_owned);
                    if let Some(ref c) = cwd {
                        command.current_dir(c);
                    }
                    command.stdout(std::process::Stdio::piped());
                    command.stderr(std::process::Stdio::piped());
                    let mut child = command.spawn().map_err(|e| e.to_string())?;
                    let mut stdout = child.stdout.take();
                    let mut stderr = child.stderr.take();
                    tokio::select! {
                        _ = cancel_c.cancelled() => {
                            let _ = child.start_kill();
                            let _ = child.wait().await;
                            Err("cancelled".to_string())
                        }
                        status = child.wait() => {
                            use tokio::io::AsyncReadExt;
                            let status = status.map_err(|e| e.to_string())?;
                            let mut body = String::new();
                            if let Some(ref mut out) = stdout {
                                let mut buf = Vec::new();
                                let _ = out.read_to_end(&mut buf).await;
                                body.push_str(&String::from_utf8_lossy(&buf));
                            }
                            if let Some(ref mut err) = stderr {
                                let mut buf = Vec::new();
                                let _ = err.read_to_end(&mut buf).await;
                                body.push_str(&String::from_utf8_lossy(&buf));
                            }
                            let clip: String = body.chars().take(400).collect();
                            if status.success() {
                                Ok(format!("completed · exit 0 · {clip}"))
                            } else {
                                Ok(format!("failed · {clip}"))
                            }
                        }
                    }
                };
                match run.await {
                    Ok(s) => s,
                    Err(e) => e,
                }
            } else {
                // Cancellable project scan (real walk with cancel polling).
                let cwd = host.inner.lock().project_cwd.clone();
                if let Some(cwd) = cwd {
                    let cancel_c = cancel.clone();
                    let host_p = host.clone();
                    let task_id_p = task_id.clone();
                    let title_p = title_for_task.clone();
                    let event_tx_p = event_tx.clone();
                    let walk = tokio::task::spawn_blocking(move || {
                        let mut n = 0usize;
                        for e in walkdir::WalkDir::new(cwd)
                            .max_depth(8)
                            .into_iter()
                            .flatten()
                        {
                            if cancel_c.is_cancelled() {
                                return Err(n);
                            }
                            if e.file_type().is_file() {
                                n += 1;
                                if n.is_multiple_of(250) {
                                    let mut g = host_p.inner.lock();
                                    if let Some(task) =
                                        g.background_tasks.iter_mut().find(|t| t.id == task_id_p)
                                    {
                                        if task.status != "cancelled" {
                                            task.detail = Some(format!("scanned {n} files…"));
                                        }
                                    }
                                    let _ = event_tx_p.send(SessionUpdate::BackgroundTask {
                                        session_id: None,
                                        task_id: task_id_p.clone(),
                                        title: title_p.clone(),
                                        status: format!("running ({n} files)"),
                                    });
                                }
                            }
                        }
                        Ok(n)
                    });
                    match walk.await {
                        Ok(Ok(n)) => format!("completed ({n} files)"),
                        Ok(Err(n)) => format!("cancelled after {n} files"),
                        Err(_) => "failed".into(),
                    }
                } else {
                    tokio::select! {
                        _ = cancel.cancelled() => "cancelled".to_string(),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {
                            "completed (no project open)".into()
                        }
                    }
                }
            };
            {
                let mut g = host.inner.lock();
                g.background_cancels.remove(&task_id);
                if let Some(task) = g.background_tasks.iter_mut().find(|t| t.id == task_id) {
                    if task.status != "cancelled" {
                        task.status = final_status.clone();
                        task.detail = Some(final_status.clone());
                    }
                }
            }
            let _ = event_tx.send(SessionUpdate::BackgroundTask {
                session_id: None,
                task_id: task_id.clone(),
                title: title_for_task,
                status: final_status,
            });
        });
        t
    }

    /// Register a long-running agent shell as a background task (visible in Tasks panel).
    pub fn register_shell_background_task(
        &self,
        call_id: &str,
        command: &str,
        session_id: Option<Uuid>,
    ) {
        let t = BackgroundTask {
            id: format!("shell-{call_id}"),
            title: command.chars().take(80).collect(),
            status: "running".into(),
            scheduled: false,
            kind: "shell".into(),
            session_id: session_id.map(|u| u.to_string()),
            detail: Some("agent tool shell".into()),
        };
        let event_tx = {
            let mut g = self.inner.lock();
            // Replace prior entry for same call id.
            g.background_tasks.retain(|x| x.id != t.id);
            g.background_tasks.push(t.clone());
            g.event_tx.clone()
        };
        let _ = event_tx.send(SessionUpdate::BackgroundTask {
            session_id,
            task_id: t.id.clone(),
            title: t.title.clone(),
            status: t.status.clone(),
        });
    }

    pub fn complete_shell_background_task(&self, call_id: &str, status: &str) {
        let id = format!("shell-{call_id}");
        let (event_tx, title) = {
            let mut g = self.inner.lock();
            let title = if let Some(task) = g.background_tasks.iter_mut().find(|t| t.id == id) {
                task.status = status.into();
                task.detail = Some(status.into());
                task.title.clone()
            } else {
                return;
            };
            (g.event_tx.clone(), title)
        };
        let _ = event_tx.send(SessionUpdate::BackgroundTask {
            session_id: None,
            task_id: id,
            title,
            status: status.into(),
        });
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
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(if s.len() > 64_000 {
            crate::textutil::truncate_with_marker(&s, 64_000, "\n…")
        } else {
            s
        })
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
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        // #166: dry-run age GC report for managed isolation worktrees
        let managed = cwd.join(".grokptah").join("worktrees");
        if managed.is_dir() {
            let report = crate::worktree_gc::gc_worktrees(
                &managed,
                crate::worktree_gc::DEFAULT_MAX_AGE,
                true,
            );
            if report.scanned > 0 {
                s.push_str(&format!(
                    "\n# auto-gc dry-run: {} aged under .grokptah/worktrees (set GROKPTAH_WORKTREE_GC=1 to delete)\n",
                    report.scanned
                ));
            }
            if std::env::var_os("GROKPTAH_WORKTREE_GC").is_some() {
                let live = crate::worktree_gc::gc_worktrees(
                    &managed,
                    crate::worktree_gc::DEFAULT_MAX_AGE,
                    false,
                );
                s.push_str(&format!("# auto-gc removed {} paths\n", live.removed.len()));
            }
        }
        Ok(s)
    }

    /// Create a git worktree under the open project (#43).
    /// `path` is relative to the project root (or absolute). `branch` is optional
    /// (new branch `-b` when provided; otherwise checkout default HEAD).
    pub fn create_worktree(&self, path: &str, branch: Option<&str>) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let path = path.trim();
        if path.is_empty() {
            bail!("worktree path is required");
        }
        let target = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            cwd.join(path)
        };
        let mut cmd = std::process::Command::new("git");
        cmd.arg("worktree").arg("add").current_dir(&cwd);
        if let Some(b) = branch.map(str::trim).filter(|b| !b.is_empty()) {
            cmd.arg("-b").arg(b);
        }
        cmd.arg(&target);
        let out = cmd.output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            bail!(
                "git worktree add failed: {}",
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            );
        }
        Ok(format!(
            "Created worktree at {}\n{}{}",
            target.display(),
            stdout,
            stderr
        ))
    }

    /// Remove a worktree path (does not delete the branch).
    pub fn remove_worktree(&self, path: &str) -> Result<String> {
        let cwd = {
            let g = self.inner.lock();
            g.project_cwd
                .clone()
                .ok_or_else(|| anyhow!("no project open"))?
        };
        let path = path.trim();
        if path.is_empty() {
            bail!("worktree path is required");
        }
        let out = std::process::Command::new("git")
            .args(["worktree", "remove", "--force", path])
            .current_dir(&cwd)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            bail!(
                "git worktree remove failed: {}",
                if stderr.trim().is_empty() {
                    stdout.trim()
                } else {
                    stderr.trim()
                }
            );
        }
        Ok(format!("{}{}", stdout, stderr))
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
        // Reconcile legacy dual-control drift so UI never shows conflicting state (#113).
        {
            let mut g = self.inner.lock();
            let bypass = g.always_approve || g.permission_mode == "bypassPermissions";
            g.always_approve = bypass;
            g.permission_mode = if bypass {
                "bypassPermissions".into()
            } else {
                "default".into()
            };
        }
        let g = self.inner.lock();
        let gw = crate::gateway_config::load();
        serde_json::json!({
            "model": g.model,
            "effort": g.effort,
            "alwaysApprove": g.always_approve,
            "sandboxProfile": g.sandbox_profile,
            "appearance": g.appearance,
            // Single effective mode for UI (mirrors alwaysApprove).
            "permissionMode": g.permission_mode,
            "effectiveToolPrompting": if g.always_approve { "bypass" } else { "prompt" },
            "allowRules": g.allow_rules,
            "denyRules": g.deny_rules,
            "autoUpdateEnabled": crate::desktop_auto_update_enabled(),
            // Corporate gateway (#169) — env overrides still win at resolve time.
            "gatewayProviderId": gw.provider_id,
            "gatewayBaseUrl": gw.base_url,
            "gatewayApiKeySet": !gw.api_key.trim().is_empty(),
        })
    }

    /// Persist OpenAI-compatible gateway settings (#169). Empty strings clear fields.
    pub fn set_gateway_config(
        &self,
        provider_id: String,
        base_url: String,
        api_key: Option<String>,
    ) -> Result<()> {
        let mut cfg = crate::gateway_config::load();
        cfg.provider_id = provider_id.trim().to_string();
        cfg.base_url = base_url.trim().to_string();
        if let Some(k) = api_key {
            let t = k.trim();
            if !t.is_empty() {
                cfg.api_key = t.to_string();
            }
        }
        crate::gateway_config::save(&cfg).map_err(|e| anyhow!("save gateway.json: {e}"))?;
        Ok(())
    }

    pub fn set_plan_mode(&self, session_id: Uuid, enabled: bool) -> Result<()> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        s.plan_mode = enabled;
        if enabled && s.plan_status.is_empty() {
            s.plan_status = "proposed".into();
        }
        Ok(())
    }

    /// Accept the proposed plan and immediately start an execution turn that
    /// follows those steps (plan → execute pipeline).
    pub async fn accept_plan(&self, session_id: Uuid) -> Result<String> {
        let (steps, goal) = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            if s.plan_steps.is_empty() {
                bail!("no plan to accept");
            }
            s.plan_mode = false;
            s.plan_status = "accepted".into();
            let steps = s.plan_steps.clone();
            let goal = s
                .plan_goal
                .clone()
                .unwrap_or_else(|| "complete the proposed plan".into());
            let tx = g.event_tx.clone();
            drop(g);
            let _ = tx.send(SessionUpdate::Plan {
                session_id,
                steps: steps.clone(),
                status: "accepted".into(),
            });
            (steps, goal)
        };

        let mut numbered = String::new();
        for (i, step) in steps.iter().enumerate() {
            numbered.push_str(&format!("{}. {}\n", i + 1, step));
        }
        let exec_prompt = format!(
            "Execute this accepted plan step by step using tools. \
             Do not re-plan unless blocked. When finished, summarize what you did.\n\n\
             Goal: {goal}\n\nPlan:\n{numbered}"
        );

        {
            let mut g = self.inner.lock();
            if let Some(s) = g.sessions.get_mut(&session_id) {
                s.plan_status = "executing".into();
            }
        }
        let reply = self.session_prompt(session_id, exec_prompt).await?;
        {
            let mut g = self.inner.lock();
            if let Some(s) = g.sessions.get_mut(&session_id) {
                s.plan_status = "done".into();
            }
            let tx = g.event_tx.clone();
            let steps = g
                .sessions
                .get(&session_id)
                .map(|s| s.plan_steps.clone())
                .unwrap_or_default();
            drop(g);
            let _ = tx.send(SessionUpdate::Plan {
                session_id,
                steps,
                status: "done".into(),
            });
        }
        Ok(reply)
    }

    pub fn reject_plan(&self, session_id: Uuid) -> Result<()> {
        let mut g = self.inner.lock();
        let s = g
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("unknown session"))?;
        s.plan_mode = false;
        s.plan_steps.clear();
        s.plan_status = "rejected".into();
        s.plan_goal = None;
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
        // AlwaysAllow is per-tool only. Global YOLO remains Settings/`set_always_approve`.
        if decision == PermissionDecision::AlwaysAllow && !pending.tool_name.is_empty() {
            g.always_allowed_tools.insert(pending.tool_name);
        }
        let _ = pending.tx.send(decision);
        Ok(())
    }

    /// Cancel the in-flight turn for `session_id`, or every active turn when
    /// `session_id` is `None` (shutdown / global stop).
    pub fn cancel_turn(&self, session_id: Option<Uuid>) -> Result<()> {
        let (live_shells, kill_ids) = {
            let mut g = self.inner.lock();
            match session_id {
                Some(id) => {
                    let Some(c) = g.turn_cancels.get(&id) else {
                        bail!("no active turn for session {id}");
                    };
                    c.cancel();
                    // #151: cancel all outstanding children for this parent session.
                    let sid = id.to_string();
                    let child_ids: Vec<String> = g
                        .subagents
                        .iter()
                        .filter(|s| {
                            s.session_id.as_deref() == Some(sid.as_str()) && s.status == "running"
                        })
                        .map(|s| s.id.clone())
                        .collect();
                    for cid in &child_ids {
                        if let Some(tok) = g.subagent_cancels.remove(cid) {
                            tok.cancel();
                        }
                        if let Some(s) = g.subagents.iter_mut().find(|s| s.id == *cid) {
                            s.status = "cancelled".into();
                            s.summary = Some("parent turn cancelled".into());
                        }
                    }
                    (g.live_shells.clone(), vec![id])
                }
                None => {
                    if g.turn_cancels.is_empty() {
                        bail!("no active turn");
                    }
                    for c in g.turn_cancels.values() {
                        c.cancel();
                    }
                    for tok in g.subagent_cancels.values() {
                        tok.cancel();
                    }
                    g.subagent_cancels.clear();
                    for s in g.subagents.iter_mut() {
                        if s.status == "running" {
                            s.status = "cancelled".into();
                            s.summary = Some("parent turn cancelled".into());
                        }
                    }
                    let ids: Vec<Uuid> = g.turn_cancels.keys().copied().collect();
                    (g.live_shells.clone(), ids)
                }
            }
        };
        // Kill only this session's (or all cancelled sessions') shell children.
        let handle = tokio::runtime::Handle::try_current();
        if let Ok(h) = handle {
            h.spawn(async move {
                let mut map = live_shells.lock().await;
                for id in kill_ids {
                    if let Some(mut child) = map.remove(&id) {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                }
            });
        } else if let Ok(mut map) = live_shells.try_lock() {
            for id in kill_ids {
                if let Some(mut child) = map.remove(&id) {
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
        self.ensure_transcript_loaded(session_id)?;
        let (cwd, model, effort, plan_mode, kind, cancel, event_tx) = {
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
            s.transcript.push(TranscriptEntry::user(prompt.clone()));
            if s.title == "New session" || s.title == "New chat" {
                s.title = prompt.chars().take(48).collect();
            }
            s.updated_at = Utc::now();
            (
                s.cwd.clone(),
                model,
                effort,
                s.plan_mode,
                s.kind,
                cancel,
                event_tx,
            )
        };
        // RAII immediately after insert — before any fallible work — so a panic
        // in persist_session cannot leave the session permanently busy.
        let mut busy_guard = TurnBusyGuard {
            host: self.clone(),
            session_id,
            armed: true,
        };
        // Durably append the user turn before the long model call.
        self.persist_session(session_id);

        let result = self
            .run_turn(
                session_id,
                &cwd,
                &model,
                effort,
                plan_mode,
                kind,
                &prompt,
                cancel.clone(),
                event_tx.clone(),
            )
            .await;

        // Normal path: disarmed so Drop is a no-op after we clean up below.
        // (We still remove here for ordering before persist + events.)
        {
            let mut g = self.inner.lock();
            g.turn_cancels.remove(&session_id);
        }
        busy_guard.armed = false;

        // Append assistant turn(s) written by push_assistant.
        self.persist_session(session_id);
        self.persist_chrome();

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

    #[allow(clippy::too_many_arguments)]
    async fn run_turn(
        &self,
        session_id: Uuid,
        cwd: &Path,
        model: &str,
        effort: EffortLevel,
        plan_mode: bool,
        kind: SessionKind,
        prompt: &str,
        cancel: CancellationToken,
        event_tx: mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }

        let lower = prompt.to_lowercase();

        // ── Regular Grok chat: conversational only (no tool loop) ─────────
        if kind == SessionKind::Chat {
            if let Some(rest) = prompt.strip_prefix('/') {
                let cmd = rest.split_whitespace().next().unwrap_or("");
                if cmd == "help" {
                    let text = "Chat mode: plain conversation with Grok. Use Builds for coding tools. /help";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
            }
            let (wire_messages, compacted_summary) = {
                let g = self.inner.lock();
                let s = g
                    .sessions
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("unknown session"))?;
                (api_context_messages(s), s.compacted_summary.clone())
            };
            let reply = if let Some(creds) = crate::auth_store::resolve_wire_credentials() {
                match call_xai_chat(
                    &creds,
                    model,
                    &wire_messages,
                    compacted_summary.as_deref(),
                    cwd,
                    SessionKind::Chat,
                )
                .await
                {
                    Ok(text) => text,
                    Err(e) => format!(
                        "Model call failed: {e}\n\nAuth: {} ({})\nRun `grok login` if needed.",
                        creds.display_name, creds.method
                    ),
                }
            } else {
                "No credentials. Run `grok login` or save an API key to chat.".into()
            };
            // One message event for live UI; invoke return is the finalize
            // source of truth (SessionPane strips streamed assistants).
            emit_message(&event_tx, session_id, &reply);
            push_assistant(self, session_id, &reply);
            return Ok(reply);
        }

        // Plan mode: propose structured steps (model-backed when online).
        if plan_mode || lower.starts_with("/plan") || lower.contains("make a plan") {
            let goal = prompt
                .strip_prefix('/')
                .and_then(|r| r.strip_prefix("plan"))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(prompt)
                .trim()
                .to_string();
            let steps = if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
                offline_plan_steps(&goal)
            } else if let Some(creds) = crate::auth_store::resolve_wire_credentials() {
                match propose_plan_with_model(&creds, model, cwd, &goal, &cancel).await {
                    Ok(s) if !s.is_empty() => s,
                    Ok(_) => offline_plan_steps(&goal),
                    Err(e) => {
                        let mut s = offline_plan_steps(&goal);
                        s.insert(0, format!("(model plan fallback: {e})"));
                        s
                    }
                }
            } else {
                offline_plan_steps(&goal)
            };

            {
                let mut g = self.inner.lock();
                if let Some(s) = g.sessions.get_mut(&session_id) {
                    s.plan_mode = true;
                    s.plan_steps = steps.clone();
                    s.plan_status = "proposed".into();
                    s.plan_goal = Some(goal.clone());
                }
            }
            let _ = event_tx.send(SessionUpdate::Plan {
                session_id,
                steps: steps.clone(),
                status: "proposed".into(),
            });
            let mut msg = String::from("Plan proposed. Accept or reject from the plan panel.\n\n");
            for (i, step) in steps.iter().enumerate() {
                msg.push_str(&format!("{}. {}\n", i + 1, step));
            }
            emit_message(&event_tx, session_id, &msg);
            push_assistant(self, session_id, &msg);
            return Ok(msg);
        }

        if let Some(rest) = prompt.strip_prefix('/') {
            let mut parts = rest.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            let args: Vec<&str> = parts.collect();
            match cmd {
                "help" => {
                    let text = "Commands: /help /compact /plan [goal] /yolo /model [id] \
                         /effort [none|low|medium|high|max] /clear /context /mcp /skills \
                         /sandbox [read-only|workspace-write|full] (tool safety profile — \
                         not an OS sandbox) /explore [query] /agents /personas /usage.\n\
                         Build mode: multi-step tool loop + optional plan accept→execute.";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                "usage" => {
                    let u = {
                        let g = self.inner.lock();
                        g.session_usage
                            .get(&session_id)
                            .cloned()
                            .unwrap_or_default()
                    };
                    // Cost: document unknown rates; report tokens only (#159).
                    let text = format!(
                        "Session usage (#159):\n\
                         - requests: {}\n\
                         - prompt_tokens: {}\n\
                         - completion_tokens: {}\n\
                         - total_tokens: {}\n\
                         Cost: not computed (no fixed rate table; see /usage in Grok Build for billed estimates).",
                        u.requests, u.prompt_tokens, u.completion_tokens, u.total_tokens
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "yolo" => {
                    self.set_always_approve(true);
                    let text = "Always-approve enabled.";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                "compact" => {
                    let before = {
                        let g = self.inner.lock();
                        g.sessions
                            .get(&session_id)
                            .map(|s| s.transcript.len())
                            .unwrap_or(0)
                    };
                    let _ = self.compact_session_async(session_id).await?;
                    let after = {
                        let g = self.inner.lock();
                        g.sessions
                            .get(&session_id)
                            .map(|s| s.transcript.len())
                            .unwrap_or(0)
                    };
                    let text = format!(
                        "Context compacted for the server. Full local history retained \
                         (local messages {before} → {after}, never decreased)."
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "model" => {
                    if let Some(id) = args.first() {
                        self.set_model((*id).to_string());
                        let text = format!("Model set to `{id}`.");
                        emit_message(&event_tx, session_id, &text);
                        push_assistant(self, session_id, &text);
                        return Ok(text);
                    }
                    let cur = self.inner.lock().model.clone();
                    let text = format!("Current model: `{cur}`. Usage: /model <id>");
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "effort" => {
                    if let Some(raw) = args.first() {
                        let e = parse_effort_arg(raw);
                        self.set_effort(e);
                        let text = format!("Effort set to `{}`.", e.as_str());
                        emit_message(&event_tx, session_id, &text);
                        push_assistant(self, session_id, &text);
                        return Ok(text);
                    }
                    let cur = self.inner.lock().effort;
                    let text = format!(
                        "Current effort: `{}`. Usage: /effort none|low|medium|high|max",
                        cur.as_str()
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "clear" => {
                    {
                        let mut g = self.inner.lock();
                        if let Some(s) = g.sessions.get_mut(&session_id) {
                            s.transcript.clear();
                            s.api_context_start = 0;
                            s.compacted_summary = None;
                            s.persisted_len = 0;
                            s.plan_mode = false;
                            s.plan_steps.clear();
                            s.plan_status.clear();
                            s.plan_goal = None;
                            s.updated_at = Utc::now();
                        }
                    }
                    self.persist_session_rewrite(session_id);
                    let text = "Session cleared (local transcript reset).";
                    emit_message(&event_tx, session_id, text);
                    push_assistant(self, session_id, text);
                    return Ok(text.into());
                }
                "context" | "cost" => {
                    let text = {
                        let g = self.inner.lock();
                        let s = g
                            .sessions
                            .get(&session_id)
                            .ok_or_else(|| anyhow!("unknown session"))?;
                        let total = s.transcript.len().max(s.persisted_len);
                        let window = total.saturating_sub(s.api_context_start);
                        format!(
                            "Context: {total} local messages · API window starts at index {} \
                             ({window} messages on wire) · model `{}` · effort `{}` · \
                             sandbox `{}` · compact summary: {} chars",
                            s.api_context_start,
                            g.model,
                            g.effort.as_str(),
                            g.sandbox_profile,
                            s.compacted_summary.as_ref().map(|c| c.len()).unwrap_or(0)
                        )
                    };
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "mcp" => {
                    let lines = self.mcp_doctor();
                    let servers = self.mcp_list();
                    let mut text = String::from("MCP servers:\n");
                    for s in &servers {
                        text.push_str(&format!(
                            "- {} [{}] enabled={} status={}\n",
                            s.name, s.transport, s.enabled, s.status
                        ));
                    }
                    if servers.is_empty() {
                        text.push_str("(none configured)\n");
                    }
                    text.push_str("\nDoctor:\n");
                    text.push_str(&lines.join("\n"));
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "skills" => {
                    let skills = self.skills();
                    let mut text = String::from("Skills:\n");
                    for s in &skills {
                        text.push_str(&format!("- **{}**: {}\n", s.name, s.description));
                    }
                    if skills.is_empty() {
                        text.push_str("(none discovered)\n");
                    }
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "agents" => {
                    let agents = self.list_agents();
                    let mut text = String::from("Agents (#164):\n");
                    for a in &agents {
                        text.push_str(&format!("- **{}**: {}\n", a.name, a.description));
                    }
                    if agents.is_empty() {
                        text.push_str(
                            "(none — add `.md` under `.grok/agents/` or `~/.grokptah/agents/`)\n",
                        );
                    }
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "personas" => {
                    let personas = self.list_personas();
                    let mut text =
                        String::from("Personas (#164) — spawn with kind `general-purpose@name`:\n");
                    for p in &personas {
                        text.push_str(&format!("- **{}**: {}\n", p.name, p.description));
                    }
                    if personas.is_empty() {
                        text.push_str(
                            "(none — add `.toml` under `.grok/personas/` or `~/.grokptah/personas/`)\n",
                        );
                    }
                    let n = self.running_subagent_count(Some(session_id));
                    if n > 0 {
                        text.push_str(&format!(
                            "\n({n} subagent{} still running)\n",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "sandbox" => {
                    // Slash alias kept for muscle memory; labeled as tool-safety
                    // profile — not an OS sandbox (#114).
                    if let Some(p) = args.first() {
                        let norm = normalize_sandbox_profile(p);
                        self.set_sandbox(norm.to_string());
                        let text = format!(
                            "Tool safety profile set to `{norm}` \
                             (agent soft gates only — not an OS sandbox)."
                        );
                        emit_message(&event_tx, session_id, &text);
                        push_assistant(self, session_id, &text);
                        return Ok(text);
                    }
                    let cur = self.inner.lock().sandbox_profile.clone();
                    let text = format!(
                        "Tool safety profile: `{cur}`.\n\
                         These are agent-side soft gates (substring denylists / \
                         tool write checks) — **not** an OS sandbox or isolation boundary.\n\
                         Profiles: `read-only` (block write tools + mutator substrings), \
                         `workspace-write` (edits allowed; block only crude escape patterns), \
                         `full` (no agent-side gates).\n\
                         Usage: /sandbox <profile>  (alias kept for compatibility)"
                    );
                    emit_message(&event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                "explore" => {
                    let query = if args.is_empty() {
                        "summarize project layout".to_string()
                    } else {
                        args.join(" ")
                    };
                    let summary = self
                        .run_explore_subagent(session_id, cwd, &query, &cancel, &event_tx)
                        .await?;
                    emit_message(&event_tx, session_id, &summary);
                    push_assistant(self, session_id, &summary);
                    return Ok(summary);
                }
                _ => {}
            }
        }

        // Offline / CI: no live model (tests set GROKPTAH_AGENT_OFFLINE=1).
        if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
            return self
                .run_offline_build_turn(session_id, cwd, prompt, &cancel, &event_tx)
                .await;
        }

        // ── Real multi-step coding agent (tool-calling loop) ─────────────
        let Some(creds) = crate::auth_store::resolve_wire_credentials() else {
            let msg = format!(
                "{}\n\nYou said: {}\nProject: {}\nModel: {} · effort: {}",
                crate::auth_store::auth_help_message(),
                prompt.chars().take(200).collect::<String>(),
                cwd.display(),
                model,
                effort.as_str()
            );
            emit_message(&event_tx, session_id, &msg);
            push_assistant(self, session_id, &msg);
            return Ok(msg);
        };

        let (wire_history, compacted_summary) = {
            let g = self.inner.lock();
            let s = g
                .sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            (api_context_messages(s), s.compacted_summary.clone())
        };

        match self
            .run_coding_agent_loop(
                session_id,
                cwd,
                model,
                effort,
                &creds,
                &wire_history,
                compacted_summary.as_deref(),
                &cancel,
                &event_tx,
            )
            .await
        {
            Ok(reply) => Ok(reply),
            Err(e) => {
                let es = e.to_string();
                surface_rate_limit_or_error(&event_tx, session_id, &es);
                let msg = format!(
                    "Agent failed: {es}\n\nAuth: {} ({})\nProject: {}\n\
                     Tips: run `grok login` if needed. If rate limited, wait before retrying.",
                    creds.display_name,
                    creds.method,
                    cwd.display()
                );
                emit_message(&event_tx, session_id, &msg);
                push_assistant(self, session_id, &msg);
                Ok(msg)
            }
        }
    }

    /// Deterministic Build turn for offline tests (no network).
    #[allow(clippy::too_many_arguments)]
    async fn run_offline_build_turn(
        &self,
        session_id: Uuid,
        cwd: &Path,
        prompt: &str,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        let lower = prompt.to_lowercase();
        if lower.contains("list") || lower.contains("files") || lower.contains("ls ") {
            let _ = self
                .run_tool_for_output(
                    session_id,
                    "list_dir",
                    &serde_json::json!({ "path": "." }),
                    || {
                        let cwd = cwd.to_path_buf();
                        async move { local_tools::tool_list_dir(&cwd, ".").await }
                    },
                    cancel,
                    event_tx,
                )
                .await;
        }
        // Offline read: "read path/to/file" — exercises tool_read_file + transcript.
        if let Some(path) = prompt
            .strip_prefix("read ")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.contains('\n'))
        {
            let _ = self
                .run_tool_for_output(
                    session_id,
                    "read_file",
                    &serde_json::json!({ "path": path }),
                    || {
                        let cwd = cwd.to_path_buf();
                        let path = path.clone();
                        async move { local_tools::tool_read_file(&cwd, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("write ") {
            if let Some((path, content)) = rest.split_once(':') {
                let path = path.trim().to_string();
                let content = content.trim().to_string();
                let path_rec = path.clone();
                if sandbox_is_readonly(&self.inner.lock().sandbox_profile) {
                    let msg = "ERROR: tool safety profile is read-only; write_file denied";
                    emit_message(event_tx, session_id, msg);
                    // still finish turn below
                } else {
                    let out = self
                        .run_tool_for_output(
                            session_id,
                            "write_file",
                            &serde_json::json!({ "path": path, "content": content }),
                            || {
                                let cwd = cwd.to_path_buf();
                                let path = path.clone();
                                let content = content.clone();
                                async move {
                                    local_tools::tool_write_file(&cwd, &path, &content).await
                                }
                            },
                            cancel,
                            event_tx,
                        )
                        .await;
                    if out.as_ref().is_ok_and(|s| !s.starts_with("DENIED")) {
                        self.emit_file_edit(
                            session_id,
                            cwd,
                            &path_rec,
                            &format!("Wrote {path_rec}"),
                            event_tx,
                        );
                    }
                }
            }
        }
        if let Some(cmd) = prompt
            .strip_prefix("run ")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            let _ = self
                .run_shell_tool_for_output(session_id, cwd, &cmd, cancel, event_tx)
                .await;
        }
        // Offline todo: "todo add buy milk" or JSON after "todo "
        if let Some(rest) = prompt.strip_prefix("todo ") {
            let args = if rest.trim_start().starts_with('{') || rest.trim_start().starts_with('[') {
                serde_json::from_str(rest).unwrap_or_else(|_| {
                    serde_json::json!({
                        "todos": [{ "id": "1", "content": rest.trim(), "status": "pending" }]
                    })
                })
            } else {
                serde_json::json!({
                    "todos": [{ "id": "1", "content": rest.trim(), "status": "pending" }]
                })
            };
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "todo_write",
                    &args.to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("remember ") {
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "memory_write",
                    &serde_json::json!({ "text": rest.trim() }).to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("recall ") {
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "memory_read",
                    &serde_json::json!({ "query": rest.trim() }).to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if let Some(rest) = prompt.strip_prefix("patch ") {
            let _ = self
                .dispatch_agent_tool(
                    session_id,
                    cwd,
                    "apply_patch",
                    &serde_json::json!({ "patch": rest.trim() }).to_string(),
                    cancel,
                    event_tx,
                    &Default::default(),
                )
                .await;
        }
        if lower.starts_with("web_fetch ") {
            if let Some(url) = prompt.split_whitespace().nth(1) {
                let _ = self
                    .dispatch_agent_tool(
                        session_id,
                        cwd,
                        "web_fetch",
                        &serde_json::json!({ "url": url }).to_string(),
                        cancel,
                        event_tx,
                        &Default::default(),
                    )
                    .await;
            }
        }
        // Prove wire context still carries compacted_summary after offline turns.
        let wire_note = {
            let g = self.inner.lock();
            g.sessions
                .get(&session_id)
                .and_then(|s| s.compacted_summary.as_ref())
                .map(|c| {
                    format!(
                        "\n[wire context includes compacted_summary: {} chars]",
                        c.len()
                    )
                })
                .unwrap_or_default()
        };
        let msg = format!(
            "(offline agent) done: {}{wire_note}",
            prompt.chars().take(80).collect::<String>()
        );
        emit_message(event_tx, session_id, &msg);
        push_assistant(self, session_id, &msg);
        Ok(msg)
    }

    /// Multi-round tool loop: model proposes tools → we run them → feed results
    /// back until a final text answer or max rounds.
    #[allow(clippy::too_many_arguments)]
    async fn run_coding_agent_loop(
        &self,
        session_id: Uuid,
        cwd: &Path,
        model: &str,
        effort: EffortLevel,
        creds: &crate::auth_store::WireCredentials,
        history: &[(String, String)],
        compacted_summary: Option<&str>,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        let max_rounds = {
            let g = self.inner.lock();
            g.max_agent_rounds
                .map(|n| n.max(1) as usize)
                .unwrap_or(24)
                .min(24)
        };
        // Auto-compact when wire window is large (non-destructive local history).
        {
            let need = {
                let g = self.inner.lock();
                g.sessions
                    .get(&session_id)
                    .map(|s| {
                        let window = s.transcript.len().saturating_sub(s.api_context_start);
                        window > 40
                    })
                    .unwrap_or(false)
            };
            if need {
                let _ = self.compact_session_async(session_id).await;
            }
        }

        let (active_plan, compacted_summary) = {
            let g = self.inner.lock();
            let s = g.sessions.get(&session_id);
            let plan = s.and_then(|s| {
                if matches!(s.plan_status.as_str(), "accepted" | "executing" | "done")
                    && !s.plan_steps.is_empty()
                {
                    Some((
                        s.plan_goal.clone().unwrap_or_else(|| "execute plan".into()),
                        s.plan_steps.clone(),
                    ))
                } else {
                    None
                }
            });
            let summary = s.and_then(|s| s.compacted_summary.clone());
            (
                plan,
                summary.or_else(|| compacted_summary.map(|s| s.to_string())),
            )
        };
        let plan_ref = active_plan
            .as_ref()
            .map(|(g, steps)| (g.as_str(), steps.as_slice()));
        let mut messages =
            build_agent_messages(history, compacted_summary.as_deref(), cwd, plan_ref);

        // Best-effort MCP discovery (skipped when offline env set for tests).
        let mcp_specs = if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some()
            || std::env::var_os("GROKPTAH_MCP_SKIP").is_some()
        {
            Vec::new()
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(8),
                crate::mcp_runtime::list_mcp_tools(Some(cwd)),
            )
            .await
            .unwrap_or_default()
        };
        let (tools, mcp_index) = coding_agent_tools(&mcp_specs);
        // #168: at most one Stop-hook continue per user turn
        let mut stop_continued = false;

        for round in 1..=max_rounds {
            if cancel.is_cancelled() {
                let msg = "(cancelled)".to_string();
                emit_message(event_tx, session_id, &msg);
                push_assistant(self, session_id, &msg);
                return Ok(msg);
            }

            // Budget-aware coaching when max_agent_rounds is tight (#187/#188).
            let remaining = max_rounds.saturating_sub(round) + 1;
            let tools_this_round = if max_rounds <= 8 && remaining == 1 {
                messages.push(serde_json::json!({
                    "role": "system",
                    "content": "BUDGET: FINAL model step. Exploration tools are disabled. Use only write_files / write_file / apply_patch / run_terminal_cmd. Apply all remaining fixes and run cargo test now.",
                }));
                // Restrict tool surface on the last step so the model cannot burn the step on list/grep/read.
                filter_tools_edit_and_shell(&tools)
            } else {
                if max_rounds <= 8 && remaining <= 2 {
                    messages.push(serde_json::json!({
                        "role": "system",
                        "content": "BUDGET: only 2 model steps left including this one. Prefer dense multi-tool edits (write_files) + cargo test over list/grep/read.",
                    }));
                }
                tools.clone()
            };

            let _ = event_tx.send(SessionUpdate::AgentProgress {
                session_id,
                round: round as u32,
                max_rounds: max_rounds as u32,
                last_tool: None,
                detail: format!("Model step {round}/{max_rounds}"),
            });

            let step = match call_xai_agent_step(
                creds,
                model,
                effort,
                &messages,
                &tools_this_round,
                cancel,
                |delta| {
                    emit_message(event_tx, session_id, delta);
                },
                |thought| {
                    emit_thought(event_tx, session_id, thought);
                },
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    surface_rate_limit_or_error(event_tx, session_id, &e.to_string());
                    return Err(e);
                }
            };

            match step {
                AgentStep::Final {
                    text,
                    streamed,
                    reasoning,
                } => {
                    if let Some(r) = reasoning.as_deref() {
                        push_thought(self, session_id, r);
                    }
                    let text = if text.trim().is_empty() {
                        if reasoning.as_ref().is_some_and(|r| !r.trim().is_empty()) {
                            // Reasoning-only turn already shown as thought; keep a thin marker.
                            String::new()
                        } else {
                            "(agent finished with empty reply)".into()
                        }
                    } else {
                        text
                    };
                    if !text.is_empty() {
                        if !streamed {
                            emit_message(event_tx, session_id, &text);
                        }
                        push_assistant(self, session_id, &text);
                    }
                    // #168 Stop hooks: optional continue with feedback (once).
                    if !stop_continued && !cancel.is_cancelled() {
                        match crate::hooks::evaluate_stop_hooks(Some(cwd)) {
                            crate::hooks::StopHookResult::ContinueWithFeedback(fb) => {
                                stop_continued = true;
                                let note = format!("(Stop hook — continuing with feedback)\n{fb}");
                                emit_message(event_tx, session_id, &note);
                                push_assistant(self, session_id, &note);
                                messages.push(serde_json::json!({
                                    "role": "user",
                                    "content": format!(
                                        "<system-reminder>Stop hook feedback: {fb}</system-reminder>"
                                    )
                                }));
                                continue;
                            }
                            crate::hooks::StopHookResult::End => {}
                        }
                    }
                    // #165: surface remaining subagents in the final line when any still run.
                    let still = self.running_subagent_count(Some(session_id));
                    let base = if text.is_empty() {
                        reasoning.unwrap_or_else(|| "(thought only)".into())
                    } else {
                        text
                    };
                    if still > 0 {
                        let note = if still == 1 {
                            format!("{base}\n\n(1 subagent still running)")
                        } else {
                            format!("{base}\n\n({still} subagents still running)")
                        };
                        return Ok(note);
                    }
                    return Ok(base);
                }
                AgentStep::ToolCalls {
                    content,
                    tool_calls,
                    streamed,
                    reasoning,
                } => {
                    if let Some(r) = reasoning.as_deref() {
                        push_thought(self, session_id, r);
                    }
                    if !streamed {
                        if let Some(c) = content.as_ref().filter(|s| !s.trim().is_empty()) {
                            emit_message(event_tx, session_id, c);
                        }
                    }

                    // OpenAI-style assistant message carrying tool_calls
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls.iter().map(|tc| serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }
                        })).collect::<Vec<_>>(),
                    }));

                    for tc in &tool_calls {
                        if cancel.is_cancelled() {
                            break;
                        }
                        let _ = event_tx.send(SessionUpdate::AgentProgress {
                            session_id,
                            round: round as u32,
                            max_rounds: max_rounds as u32,
                            last_tool: Some(tc.name.clone()),
                            detail: format!("Tool `{}` (round {round})", tc.name),
                        });
                        let output = self
                            .dispatch_agent_tool(
                                session_id,
                                cwd,
                                &tc.name,
                                &tc.arguments,
                                cancel,
                                event_tx,
                                &mcp_index,
                            )
                            .await;
                        let content = match &output {
                            Ok(s) => s.clone(),
                            Err(e) => format!("ERROR: {e}"),
                        };
                        // Cap tool output size for the wire
                        let content = if content.len() > 24_000 {
                            let orig_len = content.len();
                            format!(
                                "{}…\n(truncated {} bytes)",
                                crate::textutil::truncate_at_char_boundary(&content, 24_000),
                                orig_len
                            )
                        } else {
                            content
                        };
                        // After cargo test failures under a tight budget, force edit-next coaching.
                        if max_rounds <= 8
                            && tc.name == "run_terminal_cmd"
                            && cargo_test_output_failed(&content)
                        {
                            messages.push(serde_json::json!({
                                "role": "system",
                                "content": "cargo test reported failures. Your next step MUST apply code edits for ALL failing tests (prefer write_files or multi-file apply_patch in one step). Do not only re-read docs/README. Then re-run cargo test.",
                            }));
                        }
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": content,
                        }));
                    }
                }
            }
        }

        let msg = format!(
            "Stopped after {max_rounds} tool rounds without a final answer. \
             Ask me to continue, or narrow the task."
        );
        emit_message(event_tx, session_id, &msg);
        push_assistant(self, session_id, &msg);
        Ok(msg)
    }

    /// Emit live diff update after a successful edit tool.
    fn emit_file_edit(
        &self,
        session_id: Uuid,
        cwd: &Path,
        path: &str,
        summary: &str,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) {
        self.record_edit(path);
        let unified = crate::project_context::diff_for_path(cwd, path);
        let _ = event_tx.send(SessionUpdate::FileEdit {
            session_id,
            path: path.to_string(),
            summary: summary.to_string(),
            unified_diff: unified,
        });
    }

    /// Run one model-requested tool with permissions + UI events.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_agent_tool(
        &self,
        session_id: Uuid,
        cwd: &Path,
        name: &str,
        arguments_json: &str,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
        mcp_index: &McpToolIndex,
    ) -> Result<String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments_json).unwrap_or_else(|_| serde_json::json!({}));

        // Namespaced MCP tools
        if let Some((server, tool)) = mcp_index.get(name) {
            return self
                .run_mcp_tool(session_id, cwd, server, tool, name, &args, cancel, event_tx)
                .await;
        }

        match name {
            "list_dir" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".")
                    .to_string();
                self.run_tool_for_output(
                    session_id,
                    "list_dir",
                    &args,
                    || {
                        let cwd = cwd.to_path_buf();
                        let path = path.clone();
                        async move { local_tools::tool_list_dir(&cwd, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("read_file requires path"))?
                    .to_string();
                self.run_tool_for_output(
                    session_id,
                    "read_file",
                    &args,
                    || {
                        let cwd = cwd.to_path_buf();
                        let path = path.clone();
                        async move { local_tools::tool_read_file(&cwd, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            "grep" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("grep requires pattern"))?
                    .to_string();
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".")
                    .to_string();
                self.run_tool_for_output(
                    session_id,
                    "grep",
                    &args,
                    || {
                        let cwd = cwd.to_path_buf();
                        let pattern = pattern.clone();
                        let path = path.clone();
                        async move { local_tools::tool_grep(&cwd, &pattern, &path).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            "write_file" => {
                if sandbox_is_readonly(&self.inner.lock().sandbox_profile) {
                    return Ok("ERROR: tool safety profile is read-only; write_file denied".into());
                }
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("write_file requires path"))?
                    .to_string();
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("write_file requires content"))?
                    .to_string();
                let path_record = path.clone();
                // #146: snapshot original *before* write so FilesOnly rewind can restore.
                self.snapshot_edit_original_for_session(session_id, cwd, &path_record);
                let out = self
                    .run_tool_for_output(
                        session_id,
                        "write_file",
                        &args,
                        || {
                            let cwd = cwd.to_path_buf();
                            let path = path.clone();
                            let content = content.clone();
                            async move { local_tools::tool_write_file(&cwd, &path, &content).await }
                        },
                        cancel,
                        event_tx,
                    )
                    .await;
                if let Ok(ref report) = out {
                    self.emit_file_edit(session_id, cwd, &path_record, report, event_tx);
                }
                out
            }
            "write_files" => {
                if sandbox_is_readonly(&self.inner.lock().sandbox_profile) {
                    return Ok("ERROR: tool safety profile is read-only; write_files denied".into());
                }
                let files_val = args
                    .get("files")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| anyhow!("write_files requires files array"))?;
                let mut files: Vec<(String, String)> = Vec::new();
                for (i, item) in files_val.iter().enumerate() {
                    let path = item
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("write_files[{i}].path required"))?
                        .to_string();
                    let content = item
                        .get("content")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("write_files[{i}].content required"))?
                        .to_string();
                    self.snapshot_edit_original_for_session(session_id, cwd, &path);
                    files.push((path, content));
                }
                if files.is_empty() {
                    return Ok("ERROR: write_files files array is empty".into());
                }
                let paths: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
                let out = self
                    .run_tool_for_output(
                        session_id,
                        "write_files",
                        &args,
                        || {
                            let cwd = cwd.to_path_buf();
                            let files = files.clone();
                            async move { local_tools::tool_write_files(&cwd, &files).await }
                        },
                        cancel,
                        event_tx,
                    )
                    .await;
                if let Ok(ref report) = out {
                    for p in &paths {
                        self.emit_file_edit(session_id, cwd, p, report, event_tx);
                    }
                }
                out
            }
            "run_terminal_cmd" => {
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("run_terminal_cmd requires command"))?
                    .to_string();
                if sandbox_blocks_shell(&self.inner.lock().sandbox_profile, &command) {
                    return Ok(format!(
                        "ERROR: tool safety profile forbids this shell command \
                         (soft denylist, not an OS sandbox): {command}"
                    ));
                }
                self.run_shell_tool_for_output(session_id, cwd, &command, cancel, event_tx)
                    .await
            }
            "glob_files" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("glob_files requires pattern"))?
                    .to_string();
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
                let hits = crate::project_context::glob_files(cwd, &pattern, limit);
                let out = if hits.is_empty() {
                    "(no matches)".into()
                } else {
                    hits.join("\n")
                };
                // Emit a lightweight tool card for the UI
                let call_id = Uuid::new_v4().to_string();
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "glob_files".into(),
                    kind: ToolCallKind::Search,
                    status: ToolCallStatus::Running,
                    input: args.clone(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Completed,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "glob_files",
                    ToolCallStatus::Completed,
                    Some(out.clone()),
                );
                Ok(out)
            }
            "apply_patch" => {
                if sandbox_is_readonly(&self.inner.lock().sandbox_profile) {
                    return Ok(
                        "ERROR: tool safety profile is read-only; apply_patch denied".into(),
                    );
                }
                let patch = args
                    .get("patch")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.get("content").and_then(|v| v.as_str()))
                    .ok_or_else(|| anyhow!("apply_patch requires patch"))?
                    .to_string();
                let input = args.clone();
                let needs = true;
                let gate = self.tool_gate("apply_patch");
                if gate == ToolGate::AutoDeny {
                    return Ok("DENIED by deny rule: apply_patch".into());
                }
                let always = matches!(gate, ToolGate::AutoAllow);
                let call_id = Uuid::new_v4().to_string();
                if needs && !always {
                    let req = PermissionRequest {
                        id: Uuid::new_v4(),
                        session_id,
                        tool_name: "apply_patch".into(),
                        summary: "Allow apply_patch (edit files)?".into(),
                        detail: input.clone(),
                    };
                    let (tx, rx) = oneshot::channel();
                    {
                        let mut g = self.inner.lock();
                        g.pending_permissions.insert(
                            req.id,
                            PendingPermission {
                                tool_name: req.tool_name.clone(),
                                tx,
                            },
                        );
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
                            title: "apply_patch".into(),
                            kind: ToolCallKind::Edit,
                            status: ToolCallStatus::Denied,
                            input,
                        });
                        return Ok("DENIED: user denied apply_patch".into());
                    }
                    if decision == PermissionDecision::AlwaysAllow {
                        let mut g = self.inner.lock();
                        g.always_allowed_tools.insert("apply_patch".into());
                    }
                }
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "apply_patch".into(),
                    kind: ToolCallKind::Edit,
                    status: ToolCallStatus::Running,
                    input,
                });
                // Best-effort path hints from patch text for pre-edit snapshots (#146).
                for line in patch.lines() {
                    if let Some(p) = line
                        .strip_prefix("*** Update File: ")
                        .or_else(|| line.strip_prefix("*** Add File: "))
                    {
                        let path = p.trim();
                        if !path.is_empty() {
                            self.snapshot_edit_original_for_session(session_id, cwd, path);
                        }
                    }
                }
                match crate::project_context::apply_patch(cwd, &patch) {
                    Ok(report) => {
                        // Record + live-diff every path in the report
                        for line in report.lines() {
                            if let Some(p) = line.strip_prefix("updated ") {
                                let path = p.split(' ').next().unwrap_or("");
                                if !path.is_empty() {
                                    self.emit_file_edit(session_id, cwd, path, line, event_tx);
                                }
                            }
                        }
                        let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                            session_id,
                            call_id: call_id.clone(),
                            status: ToolCallStatus::Completed,
                            output: Some(report.clone()),
                        });
                        push_tool(
                            self,
                            session_id,
                            &call_id,
                            "apply_patch",
                            ToolCallStatus::Completed,
                            Some(report.clone()),
                        );
                        Ok(report)
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                            session_id,
                            call_id: call_id.clone(),
                            status: ToolCallStatus::Failed,
                            output: Some(msg.clone()),
                        });
                        push_tool(
                            self,
                            session_id,
                            &call_id,
                            "apply_patch",
                            ToolCallStatus::Failed,
                            Some(msg.clone()),
                        );
                        Ok(format!("ERROR: {msg}"))
                    }
                }
            }
            "spawn_explore" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("explore the codebase")
                    .to_string();
                self.run_explore_subagent(session_id, cwd, &query, cancel, event_tx)
                    .await
            }
            "spawn_general_purpose" | "spawn_subagent" => {
                let prompt = args
                    .get("prompt")
                    .or_else(|| args.get("query"))
                    .or_else(|| args.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("complete the delegated task")
                    .to_string();
                let kind = args
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("general-purpose")
                    .to_string();
                // Fire-and-forget: returns immediately so multiple children run in parallel (#151).
                self.spawn_gp_subagent_parallel(session_id, cwd, &prompt, &kind, cancel, event_tx)
            }
            "todo_write" => {
                let (items, merge) =
                    crate::todo_list::TodoList::from_tool_args(&args).map_err(|e| anyhow!(e))?;
                let rendered = {
                    let mut g = self.inner.lock();
                    let s = g
                        .sessions
                        .get_mut(&session_id)
                        .ok_or_else(|| anyhow!("unknown session"))?;
                    s.todos.apply_update(items, merge);
                    s.todos.render()
                };
                let call_id = Uuid::new_v4().to_string();
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "todo_write".into(),
                    kind: ToolCallKind::Think,
                    status: ToolCallStatus::Running,
                    input: args.clone(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Completed,
                    output: Some(rendered.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "todo_write",
                    ToolCallStatus::Completed,
                    Some(rendered.clone()),
                );
                Ok(rendered)
            }
            "memory_write" => {
                let text = args
                    .get("text")
                    .or_else(|| args.get("fact"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("memory_write requires text"))?
                    .to_string();
                let tags: Vec<String> = args
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let id = crate::memory::remember(cwd, &text, &tags).map_err(|e| anyhow!(e))?;
                let out = format!("Remembered fact {id}: {text}");
                let call_id = Uuid::new_v4().to_string();
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "memory_write".into(),
                    kind: ToolCallKind::Other,
                    status: ToolCallStatus::Completed,
                    input: args.clone(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Completed,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "memory_write",
                    ToolCallStatus::Completed,
                    Some(out.clone()),
                );
                Ok(out)
            }
            "kill_task" | "cancel_task" => {
                let id = args
                    .get("id")
                    .or_else(|| args.get("task_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    return Ok("ERROR: kill_task requires id".into());
                }
                // Prefer background task cancel; also try subagent cancel (#179).
                let bg = self.cancel_background_task(&id);
                let sub = self.cancel_subagent(&id);
                match (bg, sub) {
                    (Ok(()), _) => Ok(format!("killed background task {id}")),
                    (_, Ok(())) => Ok(format!("cancelled subagent {id}")),
                    (Err(e1), Err(e2)) => {
                        Ok(format!("ERROR: kill_task {id}: bg={e1}; subagent={e2}"))
                    }
                }
            }
            "task_output" | "get_task_output" => {
                let id = args
                    .get("id")
                    .or_else(|| args.get("task_id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let tasks = self.background_tasks();
                let subs = self.subagents();
                if let Some(id) = id {
                    if let Some(t) = tasks.iter().find(|t| t.id == id) {
                        return Ok(format!(
                            "task {} status={} kind={} detail={}",
                            t.id,
                            t.status,
                            t.kind,
                            t.detail.clone().unwrap_or_default()
                        ));
                    }
                    if let Some(s) = subs.iter().find(|s| s.id == id) {
                        return Ok(format!(
                            "subagent {} kind={} status={} summary={}",
                            s.id,
                            s.kind,
                            s.status,
                            s.summary.clone().unwrap_or_default()
                        ));
                    }
                    return Ok(format!("ERROR: unknown task/subagent id {id}"));
                }
                let mut lines = Vec::new();
                for t in &tasks {
                    lines.push(format!("task {} [{}] {}", t.id, t.status, t.title));
                }
                for s in &subs {
                    lines.push(format!("subagent {} [{}] {}", s.id, s.status, s.title));
                }
                if lines.is_empty() {
                    Ok("(no background tasks or subagents)".into())
                } else {
                    Ok(lines.join("\n"))
                }
            }
            "memory_read" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let facts = crate::memory::search(cwd, &query);
                let out = if facts.is_empty() {
                    "(no matching project memory)".into()
                } else {
                    facts
                        .iter()
                        .map(|f| format!("- {}", f.text))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let call_id = Uuid::new_v4().to_string();
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "memory_read".into(),
                    kind: ToolCallKind::Read,
                    status: ToolCallStatus::Completed,
                    input: args.clone(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Completed,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "memory_read",
                    ToolCallStatus::Completed,
                    Some(out.clone()),
                );
                Ok(out)
            }
            "web_fetch" => {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("web_fetch requires url"))?
                    .to_string();
                if sandbox_is_readonly(&self.inner.lock().sandbox_profile) {
                    return Ok("ERROR: tool safety profile is read-only; web_fetch denied".into());
                }
                self.run_tool_for_output(
                    session_id,
                    "web_fetch",
                    &args,
                    || {
                        let url = url.clone();
                        async move { tool_web_fetch(&url).await }
                    },
                    cancel,
                    event_tx,
                )
                .await
            }
            other => Ok(format!(
                "Unknown tool `{other}`. Available: list_dir, read_file, grep, write_file, write_files, \
                 run_terminal_cmd, glob_files, apply_patch, spawn_explore, spawn_general_purpose, \
                 todo_write, memory_write, memory_read, web_fetch, and mcp__* tools"
            )),
        }
    }

    /// Read-only explore subagent: gather layout/search hits and return a summary.
    async fn run_explore_subagent(
        &self,
        session_id: Uuid,
        cwd: &Path,
        query: &str,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        let sub_id = Uuid::new_v4().to_string();
        {
            let mut g = self.inner.lock();
            g.subagents.push(SubagentInfo {
                id: sub_id.clone(),
                kind: "explore".into(),
                title: query.chars().take(48).collect(),
                status: "running".into(),
                session_id: Some(session_id.to_string()),
                summary: None,
                last_tool: None,
            });
        }
        let _ = event_tx.send(SessionUpdate::SubagentSpawned {
            session_id,
            subagent_id: sub_id.clone(),
            kind: "explore".into(),
            title: query.chars().take(64).collect(),
        });

        if cancel.is_cancelled() {
            self.finish_subagent(&sub_id, "cancelled", event_tx, session_id, None);
            return Ok("(explore cancelled)".into());
        }

        // Deterministic explore: list + glob + optional grep (read-only tools).
        let listing = local_tools::tool_list_dir(cwd, ".")
            .await
            .map(|t| t.output)
            .unwrap_or_else(|e| format!("list_dir error: {e}"));
        let globs = crate::project_context::glob_files(cwd, "*.{rs,ts,tsx,js,py,md,toml,json}", 40);
        let mut parts = vec![
            format!("## Explore: {query}"),
            "### Project root listing".into(),
            listing.chars().take(4_000).collect(),
            "### Sample files".into(),
            if globs.is_empty() {
                "(no matches)".into()
            } else {
                globs.join("\n")
            },
        ];

        // Keyword grep from query tokens
        let tokens: Vec<&str> = query
            .split_whitespace()
            .filter(|t| {
                if t.len() <= 2 {
                    return false;
                }
                let l = t.to_ascii_lowercase();
                !matches!(l.as_str(), "the" | "and" | "for" | "with" | "this")
            })
            .take(3)
            .collect();
        for tok in tokens {
            if cancel.is_cancelled() {
                break;
            }
            if let Ok(tr) = local_tools::tool_grep(cwd, tok, ".").await {
                parts.push(format!(
                    "### grep `{tok}`\n{}",
                    tr.output.chars().take(2_000).collect::<String>()
                ));
            }
        }

        // Online: optional short model summary of findings
        let mut summary = parts.join("\n\n");
        if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_none() {
            if let Some(creds) = crate::auth_store::resolve_wire_credentials() {
                let model = self.inner.lock().model.clone();
                let ask = format!(
                    "You are a read-only explore agent. Summarize findings for the parent agent.\n\
                     Query: {query}\n\nFindings:\n{}",
                    summary.chars().take(8_000).collect::<String>()
                );
                if let Ok(text) = call_xai_chat(
                    &creds,
                    &model,
                    &[("user".into(), ask)],
                    None,
                    cwd,
                    SessionKind::Build,
                )
                .await
                {
                    summary = format!("{summary}\n\n### Explorer summary\n{text}");
                }
            }
        }

        let clipped: String = summary.chars().take(20_000).collect();
        self.finish_subagent(
            &sub_id,
            "completed",
            event_tx,
            session_id,
            Some(clipped.chars().take(200).collect()),
        );
        Ok(clipped)
    }

    /// Spawn a GP/plan child on a background task and return immediately (#151).
    /// Multiple spawns therefore overlap (true parallelism via JoinHandle tasks).
    fn spawn_gp_subagent_parallel(
        &self,
        session_id: Uuid,
        cwd: &Path,
        prompt: &str,
        kind: &str,
        parent_cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        // kind may be `general-purpose`, `plan`, or `kind@persona` (#164).
        let (kind, persona_name) = if let Some((k, p)) = kind.split_once('@') {
            (k.trim(), Some(p.trim()))
        } else {
            (kind.trim(), None)
        };
        let kind = if kind == "plan" {
            "plan"
        } else {
            "general-purpose"
        };
        let persona_layer = {
            let project = self.inner.lock().project_cwd.clone();
            persona_name
                .and_then(|n| crate::agents_personas::resolve_persona(project.as_deref(), n))
        };
        let sub_id = Uuid::new_v4().to_string();
        // Child is cancelled if parent is cancelled *or* cancel_subagent is called.
        let child_cancel = parent_cancel.child_token();
        let kind_label = if let Some(ref p) = persona_layer {
            format!("{}@{}", kind, p.name)
        } else {
            kind.into()
        };
        {
            let mut g = self.inner.lock();
            g.subagents.push(SubagentInfo {
                id: sub_id.clone(),
                kind: kind_label.clone(),
                title: prompt.chars().take(48).collect(),
                status: "running".into(),
                session_id: Some(session_id.to_string()),
                summary: None,
                last_tool: None,
            });
            g.subagent_cancels
                .insert(sub_id.clone(), child_cancel.clone());
        }
        let _ = event_tx.send(SessionUpdate::SubagentSpawned {
            session_id,
            subagent_id: sub_id.clone(),
            kind: kind_label.clone(),
            title: prompt.chars().take(64).collect(),
        });
        // Persist "running" row so reopen can show in-flight / history (#152).
        {
            let snap = self.inner.lock().subagents.clone();
            let _ = session_store::save_session_subagents(session_id, &snap);
        }

        let host = self.clone();
        let event_tx = event_tx.clone();
        // #162 optional worktree isolation (never empty-dir fallback)
        let mut cwd = cwd.to_path_buf();
        if std::env::var("GROKPTAH_SUBAGENT_ISOLATION")
            .map(|v| v == "worktree" || v == "1")
            .unwrap_or(false)
        {
            match crate::isolation::prepare_isolation_cwd(&cwd, &sub_id) {
                Ok(wt) => cwd = wt,
                Err(e) => {
                    // Fail closed: leave parent cwd and record failure on the child row.
                    let mut g = self.inner.lock();
                    if let Some(s) = g.subagents.iter_mut().find(|s| s.id == sub_id) {
                        s.status = "failed".into();
                        s.summary = Some(format!("isolation failed: {e}"));
                    }
                    let _ = event_tx.send(SessionUpdate::SubagentUpdate {
                        session_id,
                        subagent_id: sub_id.clone(),
                        status: "failed".into(),
                        detail: Some(format!("isolation failed: {e}")),
                    });
                    return Ok(format!(
                        "ERROR: subagent isolation failed (not starting child): {e}"
                    ));
                }
            }
        }
        let prompt = prompt.to_string();
        let kind_owned = kind.to_string();
        let persona_reminder = persona_layer
            .as_ref()
            .map(crate::agents_personas::persona_system_reminder);
        let sub_id_task = sub_id.clone();
        tokio::spawn(async move {
            host.run_gp_subagent_body(
                session_id,
                &cwd,
                &prompt,
                &kind_owned,
                &sub_id_task,
                child_cancel,
                event_tx,
                persona_reminder,
            )
            .await;
        });

        Ok(format!(
            "Spawned {kind_label} subagent `{sub_id}` (running in parallel — parent is not blocked)."
        ))
    }

    /// Body of a GP child (runs on a JoinHandle task).
    #[allow(clippy::too_many_arguments)]
    async fn run_gp_subagent_body(
        &self,
        session_id: Uuid,
        cwd: &Path,
        prompt: &str,
        kind: &str,
        sub_id: &str,
        cancel: CancellationToken,
        event_tx: mpsc::UnboundedSender<SessionUpdate>,
        persona_reminder: Option<String>,
    ) {
        if cancel.is_cancelled() {
            self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
            return;
        }

        // Offline deterministic GP: optional sleep for parallel tests + write.
        if std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some() {
            let mut parts = vec![format!("## GP subagent ({kind}): {prompt}")];
            if let Some(ref pr) = persona_reminder {
                parts.push(pr.clone());
            }
            // Parallel test hook: "sleep_ms:N ..." delays without blocking parent.
            if let Some(rest) = prompt.strip_prefix("sleep_ms:") {
                let ms: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(50);
                let sleep = tokio::time::sleep(std::time::Duration::from_millis(ms));
                tokio::pin!(sleep);
                tokio::select! {
                    _ = cancel.cancelled() => {
                        self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
                        return;
                    }
                    _ = &mut sleep => {}
                }
                parts.push(format!("### slept {ms}ms"));
            }
            if let Ok(tr) = local_tools::tool_list_dir(cwd, ".").await {
                parts.push(format!(
                    "### listing\n{}",
                    tr.output.chars().take(2_000).collect::<String>()
                ));
            }
            if let Some(rest) = prompt.find("write ").map(|i| &prompt[i + "write ".len()..]) {
                // #161: plan capability blocks mutators offline the same as online.
                if kind == "plan" || kind == "explore" {
                    parts.push(format!(
                        "### write DENIED by capability mode `{kind}`: \
                         write_file is not allowed for plan/explore children"
                    ));
                } else if let Some((path, content)) = rest.split_once(':') {
                    self.snapshot_edit_original_for_session(session_id, cwd, path.trim());
                    if let Ok(tr) =
                        local_tools::tool_write_file(cwd, path.trim(), content.trim()).await
                    {
                        parts.push(format!("### write\n{}", tr.output));
                        {
                            let mut g = self.inner.lock();
                            if let Some(s) = g.subagents.iter_mut().find(|s| s.id == sub_id) {
                                s.last_tool = Some("write_file".into());
                            }
                        }
                        self.emit_file_edit(session_id, cwd, path.trim(), &tr.output, &event_tx);
                    }
                }
            }
            if cancel.is_cancelled() {
                self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
                return;
            }
            let summary = parts.join("\n\n");
            let clipped: String = summary.chars().take(12_000).collect();
            self.finish_subagent(
                sub_id,
                "completed",
                &event_tx,
                session_id,
                Some(clipped.chars().take(400).collect()),
            );
            return;
        }

        // Online: short multi-tool agent loop under child cancel.
        let creds = match crate::auth_store::resolve_wire_credentials() {
            Some(c) => c,
            None => {
                let msg = "GP subagent: no credentials";
                self.finish_subagent(sub_id, "failed", &event_tx, session_id, Some(msg.into()));
                return;
            }
        };
        let model = self.inner.lock().model.clone();
        let effort = self.inner.lock().effort;
        let (tools, mcp_index) = coding_agent_tools(&[]);
        let mut sys = format!(
            "You are a {kind} subagent for GrokPtah. Complete the task with tools. \
             Return a concise summary for the parent when done."
        );
        if let Some(ref pr) = persona_reminder {
            sys.push('\n');
            sys.push_str(pr);
        }
        let mut messages = vec![
            serde_json::json!({
                "role": "system",
                "content": sys
            }),
            serde_json::json!({ "role": "user", "content": prompt }),
        ];
        let mut last = String::new();
        // #163: deeper child sessions (default 16 rounds; was hard-capped at 6).
        let max_child_rounds: u32 = std::env::var("GROKPTAH_SUBAGENT_MAX_ROUNDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(16)
            .clamp(4, 48);
        for _round in 1..=max_child_rounds {
            if cancel.is_cancelled() {
                self.finish_subagent(sub_id, "cancelled", &event_tx, session_id, None);
                return;
            }
            let step = call_xai_agent_step(
                &creds,
                &model,
                effort,
                &messages,
                &tools,
                &cancel,
                |_d| {},
                |_t| {},
            )
            .await;
            match step {
                Ok(AgentStep::Final {
                    text, reasoning, ..
                }) => {
                    if let Some(r) = reasoning {
                        push_thought(self, session_id, &r);
                    }
                    last = text;
                    break;
                }
                Ok(AgentStep::ToolCalls {
                    content,
                    tool_calls,
                    reasoning,
                    ..
                }) => {
                    if let Some(r) = reasoning {
                        push_thought(self, session_id, &r);
                    }
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls.iter().map(|tc| serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": { "name": tc.name, "arguments": tc.arguments }
                        })).collect::<Vec<_>>(),
                    }));
                    for tc in tool_calls {
                        if cancel.is_cancelled() {
                            break;
                        }
                        {
                            let mut g = self.inner.lock();
                            if let Some(s) = g.subagents.iter_mut().find(|s| s.id == sub_id) {
                                s.last_tool = Some(tc.name.clone());
                            }
                        }
                        // #161 capability modes: plan is non-mutating (explore is separate path).
                        let out = if tc.name.starts_with("spawn_") {
                            format!("DENIED: nested {} not allowed inside subagent", tc.name)
                        } else if kind == "plan"
                            && matches!(
                                tc.name.as_str(),
                                "write_file"
                                    | "write_files"
                                    | "apply_patch"
                                    | "run_terminal_cmd"
                                    | "memory_write"
                            )
                        {
                            format!(
                                "DENIED by capability mode `plan`: tool `{}` is not allowed. \
                                 Plan agents may only research (list/read/grep/glob) and produce a plan.",
                                tc.name
                            )
                        } else {
                            Box::pin(self.dispatch_agent_tool(
                                session_id,
                                cwd,
                                &tc.name,
                                &tc.arguments,
                                &cancel,
                                &event_tx,
                                &mcp_index,
                            ))
                            .await
                            .unwrap_or_else(|e| format!("ERROR: {e}"))
                        };
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": out.chars().take(8_000).collect::<String>(),
                        }));
                        last = out;
                    }
                }
                Err(e) => {
                    last = format!("GP subagent error: {e}");
                    break;
                }
            }
        }
        let clipped: String = last.chars().take(12_000).collect();
        self.finish_subagent(
            sub_id,
            if cancel.is_cancelled() {
                "cancelled"
            } else {
                "completed"
            },
            &event_tx,
            session_id,
            Some(clipped.chars().take(400).collect()),
        );
    }

    fn finish_subagent(
        &self,
        sub_id: &str,
        status: &str,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
        session_id: Uuid,
        detail: Option<String>,
    ) {
        let snap = {
            let mut g = self.inner.lock();
            g.subagent_cancels.remove(sub_id);
            if let Some(s) = g.subagents.iter_mut().find(|s| s.id == sub_id) {
                s.status = status.into();
                if let Some(ref d) = detail {
                    s.summary = Some(d.clone());
                }
            }
            g.subagents.clone()
        };
        let _ = session_store::save_session_subagents(session_id, &snap);
        let _ = event_tx.send(SessionUpdate::SubagentUpdate {
            session_id,
            subagent_id: sub_id.to_string(),
            status: status.into(),
            detail,
        });
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_mcp_tool(
        &self,
        session_id: Uuid,
        cwd: &Path,
        server: &str,
        tool: &str,
        wire_name: &str,
        args: &serde_json::Value,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }
        let gate = self.tool_gate(wire_name);
        if gate == ToolGate::AutoDeny {
            return Ok(format!("DENIED by deny rule: MCP `{wire_name}`"));
        }
        let always = matches!(gate, ToolGate::AutoAllow);
        let call_id = Uuid::new_v4().to_string();
        if !always {
            let req = PermissionRequest {
                id: Uuid::new_v4(),
                session_id,
                tool_name: wire_name.into(),
                summary: format!("Allow MCP tool `{server}/{tool}`?"),
                detail: args.clone(),
            };
            let (tx, rx) = oneshot::channel();
            {
                let mut g = self.inner.lock();
                g.pending_permissions.insert(
                    req.id,
                    PendingPermission {
                        tool_name: req.tool_name.clone(),
                        tx,
                    },
                );
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
                    title: wire_name.into(),
                    kind: ToolCallKind::Other,
                    status: ToolCallStatus::Denied,
                    input: args.clone(),
                });
                return Ok(format!("DENIED: user denied MCP tool `{server}/{tool}`"));
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert(wire_name.into());
            }
        }
        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: wire_name.into(),
            kind: ToolCallKind::Other,
            status: ToolCallStatus::Running,
            input: args.clone(),
        });
        let result = tokio::select! {
            r = crate::mcp_runtime::call_mcp_tool(Some(cwd), server, tool, args.clone()) => r,
            _ = cancel.cancelled() => {
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some("(cancelled)".into()),
                });
                return Ok("(cancelled)".into());
            }
        };
        match result {
            Ok(out) => {
                let clipped = if out.len() > 24_000 {
                    crate::textutil::truncate_with_marker(&out, 24_000, "…\n(truncated)")
                } else {
                    out
                };
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Completed,
                    output: Some(clipped.clone()),
                });
                Ok(clipped)
            }
            Err(e) => {
                let msg = format!("MCP error ({server}/{tool}): {e:#}");
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
                Ok(msg)
            }
        }
    }

    /// Like run_tool_call but returns the tool output string (or denial).
    async fn run_tool_for_output<F, Fut>(
        &self,
        session_id: Uuid,
        tool_name: &str,
        input: &serde_json::Value,
        f: F,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<local_tools::ToolResult>>,
    {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }

        // Tool safety profile: deny writes in read-only for shared tool path.
        if matches!(tool_name, "write_file" | "write_files" | "apply_patch")
            && sandbox_is_readonly(&self.inner.lock().sandbox_profile)
        {
            return Ok(format!(
                "ERROR: tool safety profile is read-only; {tool_name} denied"
            ));
        }

        // PreToolUse hooks can deny before permission UI / execution.
        let project = self.inner.lock().project_cwd.clone();
        if let Some(msg) = crate::hooks::pre_tool_use_deny(project.as_deref(), tool_name, input) {
            let call_id = Uuid::new_v4().to_string();
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: tool_name.into(),
                kind: tool_kind(tool_name),
                status: ToolCallStatus::Denied,
                input: input.clone(),
            });
            let out = format!("DENIED by hook: {msg}");
            let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                session_id,
                call_id,
                status: ToolCallStatus::Denied,
                output: Some(out.clone()),
            });
            return Ok(out);
        }

        let call_id = Uuid::new_v4().to_string();
        let needs_perm = matches!(
            tool_name,
            "run_terminal_cmd" | "write_file" | "write_files" | "apply_patch"
        );
        let gate = self.tool_gate(tool_name);
        if gate == ToolGate::AutoDeny {
            return Ok(format!("DENIED by deny rule: tool `{tool_name}`"));
        }
        let always = matches!(gate, ToolGate::AutoAllow);

        if needs_perm && !always {
            let req = PermissionRequest {
                id: Uuid::new_v4(),
                session_id,
                tool_name: tool_name.into(),
                summary: format!("Allow tool `{tool_name}`?"),
                detail: input.clone(),
            };
            let (tx, rx) = oneshot::channel();
            {
                let mut g = self.inner.lock();
                g.pending_permissions.insert(
                    req.id,
                    PendingPermission {
                        tool_name: req.tool_name.clone(),
                        tx,
                    },
                );
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
                    kind: tool_kind(tool_name),
                    status: ToolCallStatus::Denied,
                    input: input.clone(),
                });
                return Ok(format!("DENIED: user denied tool `{tool_name}`"));
            }
            if decision == PermissionDecision::AlwaysAllow {
                let mut g = self.inner.lock();
                g.always_allowed_tools.insert(tool_name.into());
            }
        }

        let _ = event_tx.send(SessionUpdate::ToolCall {
            session_id,
            call_id: call_id.clone(),
            title: tool_name.into(),
            kind: tool_kind(tool_name),
            status: ToolCallStatus::Running,
            input: input.clone(),
        });
        push_tool(
            self,
            session_id,
            &call_id,
            tool_name,
            ToolCallStatus::Running,
            None,
        );

        match f().await {
            Ok(tr) => {
                let out = tr.output.clone();
                let status = if tr.cancelled {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };
                let status_s = if tr.cancelled { "failed" } else { "completed" };
                let _ =
                    crate::hooks::post_tool_use_note(project.as_deref(), tool_name, status_s, &out);
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    tool_name,
                    status,
                    Some(out.clone()),
                );
                Ok(out)
            }
            Err(e) => {
                let msg = e.to_string();
                let _ =
                    crate::hooks::post_tool_use_note(project.as_deref(), tool_name, "failed", &msg);
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    tool_name,
                    ToolCallStatus::Failed,
                    Some(msg.clone()),
                );
                Ok(format!("ERROR: {msg}"))
            }
        }
    }

    async fn run_shell_tool_for_output(
        &self,
        session_id: Uuid,
        cwd: &Path,
        command: &str,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Ok("(cancelled)".into());
        }
        let call_id = Uuid::new_v4().to_string();

        // #155 exec-risk preflight (not OS sandbox)
        let risk = crate::exec_risk::assess_shell_risk(command);
        let (sandbox_profile, yolo) = {
            let g = self.inner.lock();
            (g.sandbox_profile.clone(), g.always_approve)
        };
        if risk.tier == crate::exec_risk::RiskTier::Deny
            && crate::exec_risk::should_block_deny_tier(&sandbox_profile, yolo)
        {
            let msg = format!(
                "DENIED by exec-risk: {} (peeled: `{}`). \
                 This is a tool-safety risk gate, not an OS sandbox. \
                 Adjust the command or use a full-profile YOLO session if intentional.",
                risk.reason, risk.peeled
            );
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: "run_terminal_cmd".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({
                    "command": command,
                    "risk": risk.reason,
                    "risk_tier": "deny",
                }),
            });
            push_tool(
                self,
                session_id,
                &call_id,
                "run_terminal_cmd",
                ToolCallStatus::Denied,
                Some(msg.clone()),
            );
            // #156: model-visible deny reason (tool result string)
            return Ok(msg);
        }

        let gate = self.tool_gate("run_terminal_cmd");
        if gate == ToolGate::AutoDeny {
            // #156: feed clear reason to the model
            let msg = format!(
                "DENIED by deny rule: shell `{command}` was blocked by permission deny rules. \
                 Do not retry the same command; choose a safer alternative or ask the user."
            );
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: "run_terminal_cmd".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({ "command": command }),
            });
            push_tool(
                self,
                session_id,
                &call_id,
                "run_terminal_cmd",
                ToolCallStatus::Denied,
                Some(msg.clone()),
            );
            return Ok(msg);
        }
        let always = matches!(gate, ToolGate::AutoAllow);

        // Ask-tier risk forces a prompt even under allow-rules (unless YOLO).
        let force_ask = risk.tier == crate::exec_risk::RiskTier::Ask && !yolo;
        if !always || force_ask {
            let risk_note = if risk.tier != crate::exec_risk::RiskTier::Allow {
                format!(" [risk: {}]", risk.reason)
            } else {
                String::new()
            };
            let req = PermissionRequest {
                id: Uuid::new_v4(),
                session_id,
                tool_name: "run_terminal_cmd".into(),
                summary: format!("Allow shell: {command}{risk_note}"),
                detail: serde_json::json!({
                    "tool": "run_terminal_cmd",
                    "command": command,
                    "risk": risk.reason,
                    "risk_tier": match risk.tier {
                        crate::exec_risk::RiskTier::Allow => "allow",
                        crate::exec_risk::RiskTier::Ask => "ask",
                        crate::exec_risk::RiskTier::Deny => "deny",
                    },
                    "peeled": risk.peeled,
                }),
            };
            let (tx, rx) = oneshot::channel();
            {
                let mut g = self.inner.lock();
                g.pending_permissions.insert(
                    req.id,
                    PendingPermission {
                        tool_name: req.tool_name.clone(),
                        tx,
                    },
                );
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
                let msg = format!(
                    "DENIED: user denied shell `{command}` (reason for model: do not retry; \
                     pick another approach). risk={}",
                    risk.reason
                );
                let _ = event_tx.send(SessionUpdate::ToolCall {
                    session_id,
                    call_id: call_id.clone(),
                    title: "run_terminal_cmd".into(),
                    kind: ToolCallKind::Execute,
                    status: ToolCallStatus::Denied,
                    input: serde_json::json!({ "command": command, "risk": risk.reason }),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "run_terminal_cmd",
                    ToolCallStatus::Denied,
                    Some(msg.clone()),
                );
                return Ok(msg);
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
        push_tool(
            self,
            session_id,
            &call_id,
            "run_terminal_cmd",
            ToolCallStatus::Running,
            None,
        );
        let _ = event_tx.send(SessionUpdate::ShellSessionStarted {
            session_id,
            call_id: call_id.clone(),
            command: command.to_string(),
        });
        self.register_shell_background_task(&call_id, command, Some(session_id));

        let live_shells = self.inner.lock().live_shells.clone();
        let event_tx_chunks = event_tx.clone();
        let call_id_chunks = call_id.clone();
        let result = local_tools::tool_shell_streaming(
            cwd,
            command,
            cancel.clone(),
            session_id,
            live_shells,
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
                let cancelled = tr.cancelled;
                let out = tr.output.clone();
                let status = if cancelled {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };
                self.complete_shell_background_task(
                    &call_id,
                    if cancelled { "cancelled" } else { "completed" },
                );
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code: None,
                    cancelled,
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status,
                    output: Some(out.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "run_terminal_cmd",
                    status,
                    Some(out.clone()),
                );
                Ok(if cancelled {
                    format!("{out}\n(cancelled)")
                } else {
                    out
                })
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code: None,
                    cancelled: cancel.is_cancelled(),
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
                push_tool(
                    self,
                    session_id,
                    &call_id,
                    "run_terminal_cmd",
                    ToolCallStatus::Failed,
                    Some(msg.clone()),
                );
                Ok(format!("ERROR: {msg}"))
            }
        }
    }

    /// Legacy shell helper (unused by agent loop; kept for call sites).
    #[allow(dead_code)]
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
        let risk = crate::exec_risk::assess_shell_risk(command);
        let (sandbox_profile, yolo) = {
            let g = self.inner.lock();
            (g.sandbox_profile.clone(), g.always_approve)
        };
        if risk.tier == crate::exec_risk::RiskTier::Deny
            && crate::exec_risk::should_block_deny_tier(&sandbox_profile, yolo)
        {
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: "run_terminal_cmd".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({
                    "command": command,
                    "risk": risk.reason,
                    "risk_tier": "deny",
                }),
            });
            return Ok(());
        }
        let needs_perm = true;
        let gate = self.tool_gate("run_terminal_cmd");
        if gate == ToolGate::AutoDeny {
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
        let always = matches!(gate, ToolGate::AutoAllow);
        let force_ask = risk.tier == crate::exec_risk::RiskTier::Ask && !yolo;

        if (needs_perm && !always) || force_ask {
            let risk_note = if risk.tier != crate::exec_risk::RiskTier::Allow {
                format!(" [risk: {}]", risk.reason)
            } else {
                String::new()
            };
            let req = PermissionRequest {
                id: Uuid::new_v4(),
                session_id,
                tool_name: "run_terminal_cmd".into(),
                summary: format!("Allow tool `run_terminal_cmd`?{risk_note}"),
                detail: serde_json::json!({
                    "tool": "run_terminal_cmd",
                    "command": command,
                    "risk": risk.reason,
                    "risk_tier": match risk.tier {
                        crate::exec_risk::RiskTier::Allow => "allow",
                        crate::exec_risk::RiskTier::Ask => "ask",
                        crate::exec_risk::RiskTier::Deny => "deny",
                    },
                }),
            };
            let (tx, rx) = oneshot::channel();
            {
                let mut g = self.inner.lock();
                g.pending_permissions.insert(
                    req.id,
                    PendingPermission {
                        tool_name: req.tool_name.clone(),
                        tx,
                    },
                );
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
        self.register_shell_background_task(&call_id, command, Some(session_id));

        let live_shells = self.inner.lock().live_shells.clone();
        let event_tx_chunks = event_tx.clone();
        let call_id_chunks = call_id.clone();
        let result = local_tools::tool_shell_streaming(
            cwd,
            command,
            cancel.clone(),
            session_id,
            live_shells,
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
                self.complete_shell_background_task(
                    &call_id,
                    if tr.cancelled {
                        "cancelled"
                    } else {
                        "completed"
                    },
                );
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

    #[allow(dead_code)]
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
        let needs_perm = matches!(tool_name, "run_terminal_cmd" | "write_file" | "write_files");
        let gate = self.tool_gate(tool_name);
        if gate == ToolGate::AutoDeny {
            let _ = event_tx.send(SessionUpdate::ToolCall {
                session_id,
                call_id: call_id.clone(),
                title: tool_name.into(),
                kind: ToolCallKind::Other,
                status: ToolCallStatus::Denied,
                input: serde_json::json!({ "tool": tool_name }),
            });
            return Ok(());
        }
        let always = matches!(gate, ToolGate::AutoAllow);

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
                g.pending_permissions.insert(
                    req.id,
                    PendingPermission {
                        tool_name: req.tool_name.clone(),
                        tx,
                    },
                );
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
            "write_file" | "write_files" => ToolCallKind::Edit,
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
