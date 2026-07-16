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
use crate::search_engine::{self, SearchHit, SearchQuery};
use crate::session::{Session, SessionKind, SessionSummary, TranscriptEntry};
use crate::session_store::{self, WorkspaceChrome};
use crate::types::{
    AuthState, BackgroundTask, EffortLevel, McpServerInfo, ModelInfo, PluginInfo, SkillInfo,
    SubagentInfo,
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
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            // Same source as Grok Build: config.toml [models].default, else
            // preferred id from ~/.grok/models_cache.json, else "grok-build".
            default_model: crate::models_catalog::resolve_default_model(),
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
    /// Tab strip order from the last desktop session (persisted).
    open_tab_ids: Vec<Uuid>,
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
        // Soft GC empty shells / hard cap (never delete open tabs), then reload map.
        if let Ok(n) = session_store::garbage_collect(&open_tab_ids, 80, 24 * 7) {
            if n > 0 {
                if let Ok(reloaded) = session_store::load_all_metas() {
                    sessions = reloaded;
                    open_tab_ids.retain(|id| sessions.contains_key(id));
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

    /// Persist tiny workspace chrome (tabs / project / model) only.
    pub fn persist_chrome(&self) {
        let chrome = {
            let g = self.inner.lock();
            WorkspaceChrome {
                version: 2,
                project_cwd: g
                    .project_cwd
                    .as_ref()
                    .map(|p| p.display().to_string()),
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
        if kind == SessionKind::Build && cwd.is_dir() {
            let current = self.inner.lock().project_cwd.clone();
            if current.as_ref() != Some(&cwd) {
                let _ = self.set_project_cwd(&cwd);
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

    pub fn list_sessions_by_kind(&self, kind: SessionKind, include_archived: bool) -> Vec<SessionSummary> {
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

    pub fn rewind_session(&self, id: Uuid, keep_messages: usize) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            if keep_messages < s.transcript.len() {
                s.transcript.truncate(keep_messages);
            }
            s.updated_at = Utc::now();
            s.summary()
        };
        self.persist_session_rewrite(id);
        Ok(summary)
    }

    /// Shrink the *server-facing* context window only.
    ///
    /// Local `transcript.jsonl` is never truncated or rewritten: every message
    /// stays on disk for search, UI, and perpetual history. Compact advances
    /// [`Session::api_context_start`] and stores an extractive summary of the
    /// portion that leaves the API window in [`Session::compacted_summary`].
    pub fn compact_session(&self, id: Uuid) -> Result<SessionSummary> {
        self.ensure_transcript_loaded(id)?;
        /// How many recent transcript entries stay in the wire context window.
        const KEEP_RECENT: usize = 6;
        let summary = {
            let mut g = self.inner.lock();
            let s = g
                .sessions
                .get_mut(&id)
                .ok_or_else(|| anyhow!("unknown session"))?;
            let len = s.transcript.len();
            if len > KEEP_RECENT {
                let new_start = len - KEEP_RECENT;
                // Only advance the window (re-compact moves the cut forward).
                let old_start = s.api_context_start.min(len);
                if new_start > old_start {
                    let leaving = &s.transcript[old_start..new_start];
                    let piece = build_compact_summary(leaving);
                    s.compacted_summary = Some(match s.compacted_summary.take() {
                        Some(prev) if !prev.is_empty() => format!("{prev}\n\n{piece}"),
                        _ => piece,
                    });
                    s.api_context_start = new_start;
                    let total = len;
                    let in_window = len - new_start;
                    // Additive local notice only — never deletes prior entries.
                    s.transcript.push(TranscriptEntry {
                        role: "system".into(),
                        text: format!(
                            "[context compacted for server: {in_window} recent messages stay in the API window; full local history retained ({total} messages before this notice)]"
                        ),
                    });
                }
            }
            s.updated_at = Utc::now();
            s.summary()
        };
        // Append-only notice (if any) + meta (api_context_start / summary).
        // Never rewrite_transcript — that would destroy local history.
        self.persist_session(id);
        Ok(summary)
    }

    pub fn set_model(&self, model: String) {
        self.inner.lock().model = model;
        self.persist_chrome();
    }

    pub fn set_effort(&self, effort: EffortLevel) {
        self.inner.lock().effort = effort;
        self.persist_chrome();
    }

    pub fn set_always_approve(&self, v: bool) {
        self.inner.lock().always_approve = v;
        self.persist_chrome();
    }

    pub fn set_sandbox(&self, profile: String) {
        self.inner.lock().sandbox_profile = profile;
        self.persist_chrome();
    }

    pub fn set_appearance(&self, appearance: String) {
        self.inner.lock().appearance = appearance;
        self.persist_chrome();
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
            s.transcript.push(TranscriptEntry {
                role: "user".into(),
                text: prompt.clone(),
            });
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

        {
            let mut g = self.inner.lock();
            g.turn_cancels.remove(&session_id);
        }
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
                (
                    api_context_messages(s),
                    s.compacted_summary.clone(),
                )
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
                        "Commands: /help /compact /plan /yolo. Build mode uses a multi-step tool loop \
                         (list_dir, read_file, grep, write_file, run_terminal_cmd) until the task is done.";
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
                let msg = format!(
                    "Agent failed: {e}\n\nAuth: {} ({})\nProject: {}\n\
                     Tips: run `grok login` if needed.",
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
        if let Some(rest) = prompt.strip_prefix("write ") {
            if let Some((path, content)) = rest.split_once(':') {
                let path = path.trim().to_string();
                let content = content.trim().to_string();
                let path_rec = path.clone();
                let _ = self
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
                self.record_edit(&path_rec);
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
        let msg = format!("(offline agent) done: {}", prompt.chars().take(80).collect::<String>());
        emit_message(event_tx, session_id, &msg);
        push_assistant(self, session_id, &msg);
        Ok(msg)
    }

    /// Multi-round tool loop: model proposes tools → we run them → feed results
    /// back until a final text answer or max rounds.
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
        const MAX_ROUNDS: usize = 24;
        let mut messages = build_agent_messages(history, compacted_summary, cwd);
        let tools = coding_agent_tools();

        for _round in 1..=MAX_ROUNDS {
            if cancel.is_cancelled() {
                let msg = "(cancelled)".to_string();
                emit_message(event_tx, session_id, &msg);
                push_assistant(self, session_id, &msg);
                return Ok(msg);
            }

            let step =
                call_xai_agent_step(creds, model, effort, &messages, &tools, cancel).await?;

            match step {
                AgentStep::Final(text) => {
                    let text = if text.trim().is_empty() {
                        "(agent finished with empty reply)".into()
                    } else {
                        text
                    };
                    emit_message(event_tx, session_id, &text);
                    push_assistant(self, session_id, &text);
                    return Ok(text);
                }
                AgentStep::ToolCalls {
                    content,
                    tool_calls,
                } => {
                    if let Some(c) = content.as_ref().filter(|s| !s.trim().is_empty()) {
                        // Mid-turn narration (optional)
                        emit_message(event_tx, session_id, c);
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
                        // Tool cards already surface name/status — no thought spam.
                        let output = self
                            .dispatch_agent_tool(
                                session_id,
                                cwd,
                                &tc.name,
                                &tc.arguments,
                                cancel,
                                event_tx,
                            )
                            .await;
                        let content = match &output {
                            Ok(s) => s.clone(),
                            Err(e) => format!("ERROR: {e}"),
                        };
                        // Cap tool output size for the wire
                        let content = if content.len() > 24_000 {
                            format!(
                                "{}…\n(truncated {} bytes)",
                                &content[..24_000],
                                content.len()
                            )
                        } else {
                            content
                        };
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
            "Stopped after {MAX_ROUNDS} tool rounds without a final answer. \
             Ask me to continue, or narrow the task."
        );
        emit_message(event_tx, session_id, &msg);
        push_assistant(self, session_id, &msg);
        Ok(msg)
    }

    /// Run one model-requested tool with permissions + UI events.
    async fn dispatch_agent_tool(
        &self,
        session_id: Uuid,
        cwd: &Path,
        name: &str,
        arguments_json: &str,
        cancel: &CancellationToken,
        event_tx: &mpsc::UnboundedSender<SessionUpdate>,
    ) -> Result<String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments_json).unwrap_or_else(|_| serde_json::json!({}));

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
                    return Ok("ERROR: sandbox is read-only; write_file denied".into());
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
                let out = self
                    .run_tool_for_output(
                        session_id,
                        "write_file",
                        &args,
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
                if out.is_ok() {
                    self.record_edit(&path_record);
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
                        "ERROR: sandbox profile forbids this shell command: {command}"
                    ));
                }
                self.run_shell_tool_for_output(
                    session_id,
                    cwd,
                    &command,
                    cancel,
                    event_tx,
                )
                .await
            }
            "glob_files" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("glob_files requires pattern"))?
                    .to_string();
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(80) as usize;
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
                    call_id,
                    status: ToolCallStatus::Completed,
                    output: Some(out.clone()),
                });
                Ok(out)
            }
            "apply_patch" => {
                if sandbox_is_readonly(&self.inner.lock().sandbox_profile) {
                    return Ok("ERROR: sandbox is read-only; apply_patch denied".into());
                }
                let patch = args
                    .get("patch")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.get("content").and_then(|v| v.as_str()))
                    .ok_or_else(|| anyhow!("apply_patch requires patch"))?
                    .to_string();
                let input = args.clone();
                let needs = true;
                let always = {
                    let g = self.inner.lock();
                    g.always_approve
                        || g.always_allowed_tools.contains("apply_patch")
                        || g.always_allowed_tools.contains("write_file")
                        || g.permission_mode == "bypassPermissions"
                };
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
                match crate::project_context::apply_patch(cwd, &patch) {
                    Ok(report) => {
                        // Best-effort: record first path mentioned
                        if let Some(line) = report.lines().next() {
                            if let Some(p) = line.strip_prefix("updated ") {
                                let path = p.split(' ').next().unwrap_or("");
                                if !path.is_empty() {
                                    self.record_edit(path);
                                }
                            }
                        }
                        let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                            session_id,
                            call_id,
                            status: ToolCallStatus::Completed,
                            output: Some(report.clone()),
                        });
                        Ok(report)
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                            session_id,
                            call_id,
                            status: ToolCallStatus::Failed,
                            output: Some(msg.clone()),
                        });
                        Ok(format!("ERROR: {msg}"))
                    }
                }
            }
            other => Ok(format!(
                "Unknown tool `{other}`. Available: list_dir, read_file, grep, write_file, \
                 run_terminal_cmd, glob_files, apply_patch"
            )),
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
                detail: input.clone(),
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

        match f().await {
            Ok(tr) => {
                let out = tr.output.clone();
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: if tr.cancelled {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    },
                    output: Some(out.clone()),
                });
                Ok(out)
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
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
        let always = {
            let g = self.inner.lock();
            g.always_approve
                || g.always_allowed_tools.contains("run_terminal_cmd")
                || g.permission_mode == "bypassPermissions"
        };

        if !always {
            let req = PermissionRequest {
                id: Uuid::new_v4(),
                session_id,
                tool_name: "run_terminal_cmd".into(),
                summary: format!("Allow shell: {command}"),
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
                return Ok(format!("DENIED: user denied shell `{command}`"));
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
                let cancelled = tr.cancelled;
                let out = tr.output.clone();
                let _ = event_tx.send(SessionUpdate::ShellSessionEnded {
                    session_id,
                    call_id: call_id.clone(),
                    exit_code: None,
                    cancelled,
                });
                let _ = event_tx.send(SessionUpdate::ToolCallUpdate {
                    session_id,
                    call_id,
                    status: if cancelled {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    },
                    output: Some(out.clone()),
                });
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
                    call_id,
                    status: ToolCallStatus::Failed,
                    output: Some(msg.clone()),
                });
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
    // Disk flush is append-only at turn end (session_prompt) so large replies
    // don't rewrite multi-MB files mid-stream.
}

fn emit_message(tx: &mpsc::UnboundedSender<SessionUpdate>, session_id: Uuid, text: &str) {
    let _ = tx.send(SessionUpdate::AgentMessageChunk {
        session_id,
        text: text.into(),
    });
}

#[allow(dead_code)] // reserved if we re-enable quiet diagnostics
fn emit_thought(tx: &mpsc::UnboundedSender<SessionUpdate>, session_id: Uuid, text: &str) {
    let _ = tx.send(SessionUpdate::AgentThoughtChunk {
        session_id,
        text: text.into(),
    });
}

fn tool_kind(name: &str) -> ToolCallKind {
    match name {
        "read_file" | "list_dir" => ToolCallKind::Read,
        "write_file" => ToolCallKind::Edit,
        "grep" => ToolCallKind::Search,
        "run_terminal_cmd" => ToolCallKind::Execute,
        _ => ToolCallKind::Other,
    }
}

struct AgentToolCall {
    id: String,
    name: String,
    arguments: String,
}

enum AgentStep {
    Final(String),
    ToolCalls {
        content: Option<String>,
        tool_calls: Vec<AgentToolCall>,
    },
}

fn coding_agent_tools() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List files and directories under a path relative to the project root.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative directory path. Use \".\" for the project root."
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a text file relative to the project root.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search file contents with a regex pattern under a path.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern" },
                        "path": {
                            "type": "string",
                            "description": "Relative path to search (file or directory). Default \".\""
                        }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Create or overwrite a file with the given content. Prefer complete file contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path" },
                        "content": { "type": "string", "description": "Full file contents to write" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_terminal_cmd",
                "description": "Run a shell command in the project working directory (tests, builds, git, etc.).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to execute" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "glob_files",
                "description": "Find files by glob pattern (e.g. \"*.rs\", \"src/**/*.ts\"). Returns relative paths.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob pattern" },
                        "limit": { "type": "integer", "description": "Max results (default 80)" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Apply a targeted edit. Prefer this over write_file for large files. Use either JSON {\"path\",\"old_string\",\"new_string\"} or *** Update File: path blocks with <<<<<<< SEARCH / ======= / >>>>>>> REPLACE.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch": {
                            "type": "string",
                            "description": "Patch payload (JSON search/replace or Update File blocks)"
                        }
                    },
                    "required": ["patch"]
                }
            }
        }
    ])
}

fn sandbox_is_readonly(profile: &str) -> bool {
    matches!(
        profile.trim().to_ascii_lowercase().as_str(),
        "read-only" | "readonly" | "read_only"
    )
}

fn sandbox_blocks_shell(profile: &str, command: &str) -> bool {
    if !sandbox_is_readonly(profile) {
        return false;
    }
    let c = command.to_ascii_lowercase();
    // Allow pure read-ish commands; block obvious mutators.
    let mutators = [
        "rm ", "rm\t", "mv ", "cp ", ">", ">>", "sed -i", "tee ", "npm i", "npm install",
        "cargo install", "git commit", "git push", "mkdir ", "touch ", "chmod ", "chown ",
    ];
    mutators.iter().any(|m| c.contains(m))
}

fn build_agent_messages(
    history: &[(String, String)],
    compacted_summary: Option<&str>,
    cwd: &Path,
) -> Vec<serde_json::Value> {
    let (instructions, loaded) = crate::project_context::load_project_instructions(cwd);
    let instr_note = if loaded.is_empty() {
        String::new()
    } else {
        format!("\nLoaded project instruction files: {}.\n", loaded.join(", "))
    };
    let system = format!(
        "You are GrokPtah, a desktop coding agent (Grok Build–style).\n\
         Working directory: {}.\n\
         Use tools to explore and change the codebase. Do not invent file contents — read, list, or glob first.\n\
         Prefer apply_patch for targeted edits; use write_file for new files or full rewrites.\n\
         Run tests/builds with run_terminal_cmd when useful.\n\
         When the task is done, respond with a clear final summary and no more tool calls.\n\
         Be concise in narration; put substantial content into tool arguments.{instr_note}",
        cwd.display()
    );
    let mut messages = Vec::with_capacity(history.len() + 4);
    messages.push(serde_json::json!({
        "role": "system",
        "content": system
    }));
    if !instructions.is_empty() {
        messages.push(serde_json::json!({
            "role": "system",
            "content": instructions
        }));
    }
    if let Some(summary) = compacted_summary.filter(|s| !s.is_empty()) {
        messages.push(serde_json::json!({
            "role": "system",
            "content": format!(
                "Earlier conversation was compacted for context limits \
                 (full history retained only on the user's machine).\n\n{summary}"
            )
        }));
    }
    if history.is_empty() {
        messages.push(serde_json::json!({
            "role": "user",
            "content": "(empty)"
        }));
    } else {
        for (role, content) in history {
            let role = match role.as_str() {
                "assistant" => "assistant",
                "system" => "system",
                _ => "user",
            };
            messages.push(serde_json::json!({
                "role": role,
                "content": content
            }));
        }
    }
    messages
}

fn resolve_api_base(
    creds: &crate::auth_store::WireCredentials,
    model: &str,
) -> (String, String) {
    let entry = crate::models_catalog::lookup(model);
    let model_id = entry
        .as_ref()
        .map(|e| e.wire_model.as_str())
        .unwrap_or(model)
        .to_string();
    let base = if let Ok(env) = std::env::var("XAI_API_BASE") {
        env
    } else if creds.oidc_token_auth {
        entry
            .as_ref()
            .and_then(|e| e.base_url.clone())
            .filter(|u| u.contains("cli-chat-proxy") || u.contains("x.ai"))
            .unwrap_or_else(|| "https://cli-chat-proxy.grok.com/v1".into())
    } else if let Some(u) = entry.as_ref().and_then(|e| e.base_url.clone()) {
        u
    } else {
        "https://api.x.ai/v1".into()
    };
    (base, model_id)
}

async fn call_xai_agent_step(
    creds: &crate::auth_store::WireCredentials,
    model: &str,
    effort: EffortLevel,
    messages: &[serde_json::Value],
    tools: &serde_json::Value,
    cancel: &CancellationToken,
) -> Result<AgentStep> {
    let mut creds = crate::auth_store::ensure_fresh_credentials(creds.clone()).await;
    let (base, model_id) = resolve_api_base(&creds, model);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .connect_timeout(std::time::Duration::from_secs(20))
        .user_agent(format!(
            "grok/{} (GrokPtah)",
            crate::auth_store::client_version_header()
        ))
        .build()
        .map_err(|e| anyhow!(e))?;

    // Effort on the wire (chat completions + catalog models that honor it).
    let mut body = serde_json::json!({
        "model": model_id,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "stream": false
    });
    if !matches!(effort, EffortLevel::None) {
        body["effort"] = serde_json::Value::String(effort.as_str().into());
        // Some proxy paths also read reasoning.effort
        body["reasoning"] = serde_json::json!({ "effort": effort.as_str() });
    }
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));

    // Retries for transient failures (429 / 5xx / connect).
    let mut last_err = None::<String>;
    for attempt in 0..4u32 {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        let send_once = |c: &crate::auth_store::WireCredentials| {
            let req = client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("x-grok-effort", effort.as_str());
            let req = crate::auth_store::apply_auth_headers(req, c, &base);
            req.json(&body)
        };

        let resp_result = send_once(&creds).send().await;
        let mut resp = match resp_result {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(format!("request error: {e}"));
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(400 * (1 << attempt)))
                        .await;
                    continue;
                }
                bail!("{}", last_err.unwrap());
            }
        };

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && creds.oidc_token_auth {
            match crate::auth_store::force_refresh(&creds).await {
                Ok(fresh) => {
                    creds = fresh;
                    resp = send_once(&creds)
                        .send()
                        .await
                        .map_err(|e| anyhow!("request error after refresh: {e}"))?;
                }
                Err(e) => {
                    let text = resp.text().await.unwrap_or_default();
                    bail!(
                        "HTTP 401 (refresh failed: {e}): {}",
                        text.chars().take(400).collect::<String>()
                    );
                }
            }
        }

        let status = resp.status();
        if status.as_u16() == 429
            || status.is_server_error()
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
        {
            let text = resp.text().await.unwrap_or_default();
            last_err = Some(format!(
                "HTTP {status}: {}",
                text.chars().take(400).collect::<String>()
            ));
            if attempt < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(600 * (1 << attempt)))
                    .await;
                continue;
            }
            bail!("{}", last_err.unwrap());
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "HTTP {status}: {}",
                text.chars().take(800).collect::<String>()
            );
        }

        let v: serde_json::Value = resp.json().await.map_err(|e| anyhow!("json: {e}"))?;
        let msg = &v["choices"][0]["message"];

        if let Some(arr) = msg["tool_calls"].as_array() {
            if !arr.is_empty() {
                let mut tool_calls = Vec::new();
                for tc in arr {
                    let id = tc["id"].as_str().unwrap_or("call").to_string();
                    let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    let arguments = match &tc["function"]["arguments"] {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if name.is_empty() {
                        continue;
                    }
                    tool_calls.push(AgentToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
                if !tool_calls.is_empty() {
                    let content = msg["content"].as_str().map(|s| s.to_string());
                    return Ok(AgentStep::ToolCalls {
                        content,
                        tool_calls,
                    });
                }
            }
        }

        if let Some(content) = msg["content"].as_str() {
            if !content.is_empty() {
                return Ok(AgentStep::Final(content.to_string()));
            }
        }
        if let Some(r) = msg["reasoning_content"].as_str() {
            if !r.is_empty() {
                return Ok(AgentStep::Final(r.to_string()));
            }
        }
        bail!("empty agent response: {v}");
    }
    bail!(
        "{}",
        last_err.unwrap_or_else(|| "agent request failed".into())
    );
}

/// Build the extractive summary for transcript entries that leave the API window.
fn build_compact_summary(entries: &[TranscriptEntry]) -> String {
    let mut out = String::from(
        "Summary of earlier conversation (full text is retained locally only):\n",
    );
    for (i, e) in entries.iter().enumerate() {
        let clip: String = e.text.chars().take(400).collect();
        let more = if e.text.chars().count() > 400 { "…" } else { "" };
        out.push_str(&format!("\n[{}] {}: {}{}\n", i + 1, e.role, clip, more));
    }
    const MAX: usize = 12_000;
    if out.len() > MAX {
        out.truncate(MAX);
        out.push('…');
    }
    out
}

/// Messages to send to the model: only the API context window (post-compact),
/// excluding local system notices. Never includes the truncated local prefix.
fn api_context_messages(session: &Session) -> Vec<(String, String)> {
    let start = session.api_context_start.min(session.transcript.len());
    session.transcript[start..]
        .iter()
        .filter(|e| e.role == "user" || e.role == "assistant")
        .filter(|e| !e.text.starts_with("[context compacted for server:"))
        .map(|e| (e.role.clone(), e.text.clone()))
        .collect()
}

/// Call the chat completions API.
///
/// `history` is already windowed (post-`api_context_start`); last entry is
/// typically the current user prompt. `compacted_summary` is the extractive
/// stand-in for local-only prefix that left the context window.
async fn call_xai_chat(
    creds: &crate::auth_store::WireCredentials,
    model: &str,
    history: &[(String, String)],
    compacted_summary: Option<&str>,
    cwd: &Path,
    kind: SessionKind,
) -> Result<String> {
    // Prefer a non-expired / refreshed OIDC access token before the first call.
    let mut creds = crate::auth_store::ensure_fresh_credentials(creds.clone()).await;

    // Use the same catalog Grok Build wrote (wire id + base_url). Do not remap
    // `grok-build` → `grok-3` — that made the desktop look stuck on old models.
    let entry = crate::models_catalog::lookup(model);
    let model_id = entry
        .as_ref()
        .map(|e| e.wire_model.as_str())
        .unwrap_or(model)
        .to_string();
    // OIDC / cli session: prefer the proxy base from models_cache (cli-chat-proxy).
    // API keys: public api.x.ai unless the catalog specifies otherwise or env overrides.
    // Always force cli-chat-proxy for OIDC user tokens — api.x.ai rejects them.
    let base = if let Ok(env) = std::env::var("XAI_API_BASE") {
        env
    } else if creds.oidc_token_auth {
        entry
            .as_ref()
            .and_then(|e| e.base_url.clone())
            .filter(|u| u.contains("cli-chat-proxy") || u.contains("x.ai"))
            .unwrap_or_else(|| "https://cli-chat-proxy.grok.com/v1".into())
    } else if let Some(u) = entry.as_ref().and_then(|e| e.base_url.clone()) {
        u
    } else {
        "https://api.x.ai/v1".into()
    };
    let system = match kind {
        SessionKind::Chat => {
            "You are Grok, a helpful, witty AI assistant in GrokPtah. \
             This is a regular conversation — not a coding-agent build session. \
             Answer clearly; use markdown when useful. Do not invent local file edits."
                .to_string()
        }
        SessionKind::Build => format!(
            "You are GrokPtah, a desktop coding agent built on Grok Build. \
             Working directory: {}. Be helpful and concise. Prefer concrete code changes.",
            cwd.display()
        ),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(20))
        .user_agent(format!(
            "grok/{} (GrokPtah)",
            crate::auth_store::client_version_header()
        ))
        .build()
        .map_err(|e| anyhow!(e))?;

    let mut messages = Vec::with_capacity(history.len() + 2);
    messages.push(serde_json::json!({
        "role": "system",
        "content": system
    }));
    if let Some(summary) = compacted_summary.filter(|s| !s.is_empty()) {
        // Carry condensed prior context on the wire without re-sending full local log.
        messages.push(serde_json::json!({
            "role": "system",
            "content": format!(
                "The conversation was compacted for context limits. \
                 Full history is retained only on the user's machine.\n\n{summary}"
            )
        }));
    }
    if history.is_empty() {
        // Fallback: should not happen once the user turn is on the transcript.
        messages.push(serde_json::json!({
            "role": "user",
            "content": "(empty)"
        }));
    } else {
        for (role, content) in history {
            let role = match role.as_str() {
                "assistant" => "assistant",
                "system" => "system",
                _ => "user",
            };
            messages.push(serde_json::json!({
                "role": role,
                "content": content
            }));
        }
    }

    let body = serde_json::json!({
        "model": model_id,
        "messages": messages,
        "stream": false
    });
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));

    let send_once = |c: &crate::auth_store::WireCredentials| {
        let req = client
            .post(&url)
            .header("Content-Type", "application/json");
        let req = crate::auth_store::apply_auth_headers(req, c, &base);
        req.json(&body)
    };

    let mut resp = send_once(&creds).send().await.map_err(|e| {
        // Surface classify-able transport failures (DNS, TLS, timeout) so the
        // UI is not a vague "error sending request".
        let kind = if e.is_timeout() {
            "timeout"
        } else if e.is_connect() {
            "connect"
        } else if e.is_request() {
            "request"
        } else {
            "network"
        };
        anyhow!(
            "request error ({kind}) for {url}: {e}. \
             Check network, VPN, and that cli-chat-proxy is reachable."
        )
    })?;

    // One retry after OIDC refresh on 401 (expired access token is common).
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED && creds.oidc_token_auth {
        match crate::auth_store::force_refresh(&creds).await {
            Ok(fresh) => {
                creds = fresh;
                resp = send_once(&creds).send().await.map_err(|e| {
                    anyhow!("request error after refresh for {url}: {e}")
                })?;
            }
            Err(e) => {
                let text = resp.text().await.unwrap_or_default();
                let clipped: String = text.chars().take(400).collect();
                bail!(
                    "HTTP 401 Unauthorized (refresh also failed: {e}). \
                     Server said: {clipped}. Run `grok login` to re-authenticate."
                );
            }
        }
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let clipped: String = text.chars().take(800).collect();
        bail!("HTTP {status}: {clipped}");
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| anyhow!("json: {e}"))?;
    // chat/completions shape
    if let Some(content) = v["choices"][0]["message"]["content"].as_str() {
        if !content.is_empty() {
            return Ok(content.to_string());
        }
    }
    // responses API fallback (some catalog models use this backend)
    if let Some(content) = v["output_text"].as_str() {
        if !content.is_empty() {
            return Ok(content.to_string());
        }
    }
    if let Some(arr) = v["output"].as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(t) = item["content"][0]["text"].as_str() {
                parts.push(t.to_string());
            }
        }
        if !parts.is_empty() {
            return Ok(parts.join(""));
        }
    }
    bail!("empty model response: {v}");
}

