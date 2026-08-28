//! Robust on-disk session storage for large / long-lived chats.
//!
//! ## Layout (`~/.grokptah/`)
//! ```text
//! workspace.json                 # small chrome only (tabs, project, model)
//! sessions/<uuid>/
//!   meta.json                    # title, cwd, counts, plan — not the chat body
//!   transcript.jsonl             # append-only one TranscriptEntry per line
//! ```
//!
//! Why not a single workspace.json with full transcripts?
//! - Rewriting multi‑MB JSON on every tab switch / token is slow and lossy on crash
//! - One corrupt session must not take down the whole store
//! - Append-only JSONL matches how Grok Build keeps conversation logs
//!
//! ## Write strategy
//! - **Chrome** (`workspace.json`): rewrite atomically (tiny)
//! - **Meta**: rewrite atomically when title/model/plan changes
//! - **Transcript**: *append* new lines; full rewrite only on rewind/compact
//! - **Lazy load**: metas load at boot; JSONL loads when a session is opened
//!
//! Migrates legacy v1 `workspace.json` that embedded full sessions (one-shot).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::discover::{ensure_home, grokptah_home};
use crate::orchestration::RunExecutionMode;
use crate::session::{Session, TranscriptEntry};
use crate::types::{EffortLevel, SubagentIsolationPreference};

const STORE_VERSION: u32 = 2;
const SESSION_CREATION_INTENT_FILE: &str = "session-create-intent.json";

#[cfg(test)]
static TEST_PERSISTENCE_FAILURE: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
pub(crate) fn set_test_persistence_failure(point: Option<&str>) {
    let value = match point {
        None => 0,
        Some("transcript") => 1,
        Some("meta") => 2,
        Some("chrome") => 3,
        Some("write") => 4,
        Some("file_sync") => 5,
        Some("rename") => 6,
        Some("dir_sync") => 7,
        Some("intent_remove") => 8,
        Some(other) => panic!("unknown persistence failure point: {other}"),
    };
    TEST_PERSISTENCE_FAILURE.store(value, Ordering::Release);
}

#[cfg(test)]
fn fail_test_persistence(point: &str) -> Result<()> {
    let value = match point {
        "transcript" => 1,
        "meta" => 2,
        "chrome" => 3,
        "write" => 4,
        "file_sync" => 5,
        "rename" => 6,
        "dir_sync" => 7,
        "intent_remove" => 8,
        _ => 0,
    };
    if value != 0
        && TEST_PERSISTENCE_FAILURE
            .compare_exchange(value, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        bail!("injected persistence failure at boundary {point}");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCommitStatus {
    Committed,
    RecoveryRequired,
}

// ── Workspace chrome (always small) ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceChrome {
    pub version: u32,
    pub project_cwd: Option<String>,
    pub active_session: Option<Uuid>,
    #[serde(default)]
    pub open_tab_ids: Vec<Uuid>,
    pub model: String,
    pub effort: EffortLevel,
    #[serde(default)]
    pub sandbox_profile: String,
    #[serde(default)]
    pub appearance: String,
    #[serde(default)]
    pub always_approve: bool,
    #[serde(default)]
    pub subagent_isolation: SubagentIsolationPreference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionCreationIntent {
    session_id: Uuid,
    #[serde(default)]
    prior_chrome: Option<Vec<u8>>,
    next_chrome: WorkspaceChrome,
}

impl Default for WorkspaceChrome {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            project_cwd: None,
            active_session: None,
            open_tab_ids: Vec::new(),
            model: crate::models_catalog::resolve_default_model(),
            effort: EffortLevel::Medium,
            sandbox_profile: "workspace-write".into(),
            appearance: "dark".into(),
            always_approve: false,
            subagent_isolation: SubagentIsolationPreference::Worktree,
        }
    }
}

// ── Per-session metadata (no transcript body) ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: Uuid,
    #[serde(default)]
    pub agent_id: Option<String>,
    pub title: String,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub forked_from: Option<Uuid>,
    pub model: String,
    pub effort: EffortLevel,
    #[serde(default)]
    pub plan_mode: bool,
    #[serde(default)]
    pub plan_steps: Vec<String>,
    #[serde(default)]
    pub plan_status: String,
    #[serde(default)]
    pub plan_goal: Option<String>,
    #[serde(default)]
    pub compacted_summary: Option<String>,
    /// Index into transcript.jsonl where the API context window begins.
    /// Compact advances this; local lines before it are never deleted.
    #[serde(default)]
    pub api_context_start: usize,
    /// Number of lines in transcript.jsonl (authoritative for list badges).
    #[serde(default)]
    pub message_count: usize,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub kind: crate::session::SessionKind,
    #[serde(default)]
    pub execution_mode: RunExecutionMode,
    #[serde(default)]
    pub completion_history: Vec<crate::session::SessionCompletion>,
}

// ── Paths ───────────────────────────────────────────────────────────────────

pub fn sessions_root() -> PathBuf {
    grokptah_home().join("sessions")
}

pub fn chrome_path() -> PathBuf {
    grokptah_home().join("workspace.json")
}

pub fn session_dir(id: Uuid) -> PathBuf {
    sessions_root().join(id.to_string())
}

fn meta_path(id: Uuid) -> PathBuf {
    session_dir(id).join("meta.json")
}

fn transcript_path(id: Uuid) -> PathBuf {
    session_dir(id).join("transcript.jsonl")
}

fn subagents_path(id: Uuid) -> PathBuf {
    session_dir(id).join("subagents.json")
}

fn prompt_queue_path(id: Uuid) -> PathBuf {
    session_dir(id).join("prompt_queue.json")
}

/// Persist bridge-owned prompt queue entries so ordering survives restart (#196).
pub fn save_prompt_queue(id: Uuid, queue: &crate::prompt_queue::SessionPromptQueue) -> Result<()> {
    ensure_home();
    let _ = fs::create_dir_all(session_dir(id));
    let path = prompt_queue_path(id);
    let snap = queue.durable_snapshot();
    atomic_write_json(&path, &snap)
}

/// Load durable prompt queue for a session (empty if missing).
pub fn load_prompt_queue(id: Uuid) -> Result<crate::prompt_queue::SessionPromptQueue> {
    let path = prompt_queue_path(id);
    if !path.is_file() {
        return Ok(crate::prompt_queue::SessionPromptQueue::default());
    }
    let text = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

/// Load all persisted queues for known sessions.
pub fn load_all_prompt_queues(
    session_ids: impl IntoIterator<Item = Uuid>,
) -> HashMap<Uuid, crate::prompt_queue::SessionPromptQueue> {
    let mut out = HashMap::new();
    for id in session_ids {
        if let Ok(q) = load_prompt_queue(id) {
            if !q.list().is_empty() {
                out.insert(id, q);
            }
        }
    }
    out
}

/// Persist subagent history for a session (reopen / historical summary) (#152).
pub fn save_session_subagents(id: Uuid, list: &[crate::types::SubagentInfo]) -> Result<()> {
    ensure_home();
    let _ = fs::create_dir_all(session_dir(id));
    // Keep only rows for this session (and rows without session_id for safety).
    let filtered: Vec<_> = list
        .iter()
        .filter(|s| {
            s.session_id
                .as_deref()
                .map(|sid| sid == id.to_string())
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    let path = subagents_path(id);
    atomic_write_bytes(&path, &serde_json::to_vec_pretty(&filtered)?)
}

pub fn load_session_subagents(id: Uuid) -> Vec<crate::types::SubagentInfo> {
    let path = subagents_path(id);
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Load chrome + session shells (transcripts empty until [`load_transcript`]).
pub fn load_workspace() -> Result<(WorkspaceChrome, HashMap<Uuid, Session>)> {
    ensure_home();
    let _ = fs::create_dir_all(sessions_root());

    recover_session_creation_intent()?;
    // One-shot migration from monolithic v1 file.
    migrate_v1_if_needed()?;

    let chrome = load_chrome().unwrap_or_default();
    let sessions = load_all_metas()?;
    Ok((chrome, sessions))
}

pub fn save_chrome(chrome: &WorkspaceChrome) -> Result<()> {
    ensure_home();
    let mut c = chrome.clone();
    c.version = STORE_VERSION;
    atomic_write_json(&chrome_path(), &c)
}

pub(crate) fn restore_chrome_snapshot(snapshot: Option<&[u8]>) -> Result<()> {
    match snapshot {
        Some(bytes) => atomic_write_bytes(&chrome_path(), bytes),
        None => remove_file_durable(&chrome_path()),
    }
}

pub fn save_session_meta(session: &Session) -> Result<()> {
    let dir = session_dir(session.id);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let meta = SessionMeta::from_session(session);
    #[cfg(test)]
    fail_test_persistence("meta")?;
    atomic_write_json(&meta_path(session.id), &meta)
}

/// Persist a new session and its chrome publication as one recoverable
/// transaction. The session is not visible to a restarted host unless both
/// its durable files and the chrome pointer were committed.
pub fn create_session_durable(
    session: &Session,
    next_chrome: &WorkspaceChrome,
) -> Result<SessionCommitStatus> {
    ensure_home();
    let dir = session_dir(session.id);
    if dir.exists() {
        bail!("session id already exists");
    }
    let prior_chrome = match fs::read(chrome_path()) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let intent = SessionCreationIntent {
        session_id: session.id,
        prior_chrome,
        next_chrome: next_chrome.clone(),
    };
    let intent_path = grokptah_home().join(SESSION_CREATION_INTENT_FILE);
    atomic_write_json(&intent_path, &intent)?;

    let result = (|| {
        #[cfg(test)]
        fail_test_persistence("transcript")?;
        rewrite_transcript(session)?;
        #[cfg(test)]
        fail_test_persistence("chrome")?;
        save_chrome(next_chrome)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            #[cfg(test)]
            if fail_test_persistence("intent_remove").is_err() {
                return Ok(SessionCommitStatus::RecoveryRequired);
            }
            match remove_file_durable(&intent_path) {
                Ok(()) => Ok(SessionCommitStatus::Committed),
                Err(error) => {
                    eprintln!(
                        "[grokptah] session creation committed; intent cleanup deferred: {error:#}"
                    );
                    Ok(SessionCommitStatus::RecoveryRequired)
                }
            }
        }
        Err(error) => {
            let rollback = rollback_session_creation(&intent);
            if let Err(rollback_error) = rollback {
                return Err(anyhow::anyhow!(
                    "{error:#}; session creation rollback failed: {rollback_error:#}"
                ));
            }
            remove_file_durable(&intent_path).context("remove failed session creation intent")?;
            Err(error)
        }
    }
}

/// Append transcript entries that are not yet on disk (`from_index..`).
/// Returns how many lines were written.
///
/// Each entry is serialized to a complete line string first, then written as a
/// single `write_all` so a crash cannot leave a half-encoded JSON object
/// mid-line (#118).
pub fn append_transcript(session: &Session, from_index: usize) -> Result<usize> {
    if from_index >= session.transcript.len() {
        return Ok(0);
    }
    let dir = session_dir(session.id);
    fs::create_dir_all(&dir)?;
    let path = transcript_path(session.id);
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open append {}", path.display()))?;
    let mut n = 0;
    let mut batch = String::new();
    for entry in session.transcript.iter().skip(from_index) {
        let line = serde_json::to_string(entry).with_context(|| "serialize transcript entry")?;
        batch.push_str(&line);
        batch.push('\n');
        n += 1;
    }
    f.write_all(batch.as_bytes())
        .with_context(|| format!("write append {}", path.display()))?;
    f.flush()?;
    #[cfg(test)]
    fail_test_persistence("file_sync")?;
    f.sync_all()
        .with_context(|| format!("sync append {}", path.display()))?;
    // Keep meta.message_count in sync
    save_session_meta(session)?;
    Ok(n)
}

/// Full rewrite of transcript.jsonl (rewind / fork only — never compact).
/// Compact must not call this: local history is append-only forever.
pub fn rewrite_transcript(session: &Session) -> Result<()> {
    let dir = session_dir(session.id);
    fs::create_dir_all(&dir)?;
    let path = transcript_path(session.id);
    let mut batch = String::new();
    for entry in &session.transcript {
        let line = serde_json::to_string(entry)?;
        batch.push_str(&line);
        batch.push('\n');
    }
    atomic_write_bytes(&path, batch.as_bytes())
        .with_context(|| format!("rewrite transcript {}", path.display()))?;
    save_session_meta(session)?;
    Ok(())
}

/// Load full transcript into a session shell. No-op if already loaded.
///
/// A torn **trailing** line (crash mid-append) is skipped with a log rather
/// than making the whole session unopenable (#118).
pub fn load_transcript(session: &mut Session) -> Result<()> {
    if session.transcript_loaded {
        return Ok(());
    }
    let path = transcript_path(session.id);
    if !path.is_file() {
        session.transcript.clear();
        session.transcript_loaded = true;
        session.persisted_len = 0;
        return Ok(());
    }
    let f = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(f);
    let mut entries = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {} of {}", i + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        lines.push(line);
    }
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<TranscriptEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                if i == last {
                    eprintln!(
                        "[grokptah] skip torn trailing transcript line in {}: {e}",
                        path.display()
                    );
                } else {
                    // Mid-file corruption is rarer; still skip rather than brick the session.
                    eprintln!(
                        "[grokptah] skip corrupt transcript line {} in {}: {e}",
                        i + 1,
                        path.display()
                    );
                }
            }
        }
    }
    session.transcript = entries;
    session.transcript_loaded = true;
    session.persisted_len = session.transcript.len();
    Ok(())
}

/// Delete a session directory (optional GC / close-and-forget).
pub fn delete_session(id: Uuid) -> Result<()> {
    let dir = session_dir(id);
    if dir.is_dir() {
        fs::remove_dir_all(&dir).with_context(|| format!("rm -rf {}", dir.display()))?;
        if let Some(parent) = dir.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

/// Soft GC: drop empty "New session" shells older than `max_age` and cap
/// **active** (non-archived) session dirs. Never deletes open tabs or archived.
pub fn garbage_collect(
    open_ids: &[Uuid],
    max_sessions: usize,
    max_empty_age_hours: i64,
) -> Result<usize> {
    let mut metas = list_session_metas()?;
    if metas.is_empty() {
        return Ok(0);
    }
    let open: std::collections::HashSet<_> = open_ids.iter().copied().collect();
    let now = Utc::now();
    let mut removed = 0usize;

    // 1) Empty new sessions older than threshold (never archived)
    for m in &metas {
        if open.contains(&m.id) || m.archived {
            continue;
        }
        if m.message_count == 0
            && m.title == "New session"
            && (now - m.updated_at).num_hours() >= max_empty_age_hours
        {
            delete_session(m.id)?;
            removed += 1;
        }
    }

    // 2) Cap active (non-archived) count
    metas = list_session_metas()?;
    let mut active: Vec<_> = metas.into_iter().filter(|m| !m.archived).collect();
    if active.len() > max_sessions {
        active.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        let overflow = active.len() - max_sessions;
        for m in active.into_iter().take(overflow) {
            if open.contains(&m.id) {
                continue;
            }
            delete_session(m.id)?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn cwd_still_valid(cwd: Option<&str>) -> Option<PathBuf> {
    let s = cwd?;
    let p = Path::new(s);
    if p.is_dir() {
        Some(p.to_path_buf())
    } else {
        None
    }
}

// ── Internals ───────────────────────────────────────────────────────────────

impl SessionMeta {
    fn from_session(s: &Session) -> Self {
        Self {
            id: s.id,
            agent_id: s.agent_id.clone(),
            title: s.title.clone(),
            cwd: s.cwd.display().to_string(),
            created_at: s.created_at,
            updated_at: s.updated_at,
            forked_from: s.forked_from,
            model: s.model.clone(),
            effort: s.effort,
            plan_mode: s.plan_mode,
            plan_steps: s.plan_steps.clone(),
            plan_status: s.plan_status.clone(),
            plan_goal: s.plan_goal.clone(),
            compacted_summary: s.compacted_summary.clone(),
            api_context_start: s.api_context_start,
            message_count: s.transcript.len().max(s.persisted_len),
            folder: s.folder.clone(),
            tags: s.tags.clone(),
            archived: s.archived,
            archived_at: s.archived_at,
            kind: s.kind,
            execution_mode: s.execution_mode,
            completion_history: s.completion_history.clone(),
        }
    }

    fn into_shell(self) -> Session {
        Session {
            id: self.id,
            agent_id: self.agent_id,
            title: self.title,
            cwd: PathBuf::from(self.cwd),
            created_at: self.created_at,
            updated_at: self.updated_at,
            transcript: Vec::new(),
            forked_from: self.forked_from,
            model: self.model,
            effort: self.effort,
            plan_mode: self.plan_mode,
            plan_steps: self.plan_steps,
            plan_status: self.plan_status,
            plan_goal: self.plan_goal,
            compacted_summary: self.compacted_summary,
            api_context_start: self.api_context_start,
            folder: self.folder,
            tags: self.tags,
            archived: self.archived,
            archived_at: self.archived_at,
            kind: self.kind,
            execution_mode: self.execution_mode,
            completion_history: self.completion_history,
            transcript_loaded: false,
            // Until load_transcript, treat disk as authoritative length.
            persisted_len: self.message_count,
            todos: crate::todo_list::TodoList::default(),
        }
    }
}

fn load_chrome() -> Result<WorkspaceChrome> {
    let path = chrome_path();
    if !path.is_file() {
        return Ok(WorkspaceChrome::default());
    }
    let raw = fs::read_to_string(&path)?;
    // Reject legacy v1 full dumps (have "sessions" array) — migrate handles that.
    if raw.contains("\"sessions\"") && !sessions_root().is_dir() {
        bail!("legacy v1 workspace pending migration");
    }
    let mut c: WorkspaceChrome = serde_json::from_str(&raw)?;
    c.version = STORE_VERSION;
    Ok(c)
}

fn list_session_metas() -> Result<Vec<SessionMeta>> {
    let root = sessions_root();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let meta_p = entry.path().join("meta.json");
        if !meta_p.is_file() {
            continue;
        }
        let raw = fs::read_to_string(&meta_p)?;
        match serde_json::from_str::<SessionMeta>(&raw) {
            Ok(m) => out.push(m),
            Err(e) => {
                eprintln!(
                    "[grokptah] skip corrupt session meta {}: {e}",
                    meta_p.display()
                );
            }
        }
    }
    Ok(out)
}

pub fn load_all_metas() -> Result<HashMap<Uuid, Session>> {
    let mut map = HashMap::new();
    for m in list_session_metas()? {
        map.insert(m.id, m.into_shell());
    }
    Ok(map)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let raw = serde_json::to_string_pretty(value)?;
    atomic_write_bytes(path, raw.as_bytes())
}

fn remove_file_durable(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = path.parent() {
        if let Ok(dirf) = File::open(parent) {
            dirf.sync_all()?;
        }
    }
    Ok(())
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let result = (|| {
        #[cfg(test)]
        fail_test_persistence("write")?;
        {
            let mut file =
                File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            file.write_all(bytes)
                .with_context(|| format!("write {}", tmp.display()))?;
            file.flush()?;
            #[cfg(test)]
            fail_test_persistence("file_sync")?;
            file.sync_all()
                .with_context(|| format!("sync {}", tmp.display()))?;
        }
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        #[cfg(test)]
        fail_test_persistence("rename")?;
        fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn sync_directory(path: &Path) -> Result<()> {
    let directory =
        File::open(path).with_context(|| format!("open directory {}", path.display()))?;
    #[cfg(test)]
    fail_test_persistence("dir_sync")?;
    directory
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

fn rollback_session_creation(intent: &SessionCreationIntent) -> Result<()> {
    let mut first_error = None;
    if let Err(error) = delete_session(intent.session_id) {
        first_error = Some(error);
    }
    let chrome_result = match intent.prior_chrome.as_deref() {
        Some(bytes) => atomic_write_bytes(&chrome_path(), bytes),
        None => remove_file_durable(&chrome_path()),
    };
    if let Err(error) = chrome_result {
        if first_error.is_none() {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn recover_session_creation_intent() -> Result<()> {
    let intent_path = grokptah_home().join(SESSION_CREATION_INTENT_FILE);
    if !intent_path.is_file() {
        return Ok(());
    }
    let intent: SessionCreationIntent = serde_json::from_slice(&fs::read(&intent_path)?)
        .context("parse session creation intent")?;
    let session_complete = session_dir(intent.session_id).join("meta.json").is_file()
        && session_dir(intent.session_id)
            .join("transcript.jsonl")
            .is_file();
    let chrome_complete = fs::read(chrome_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WorkspaceChrome>(&bytes).ok())
        .is_some_and(|chrome| {
            serde_json::to_vec(&chrome).ok() == serde_json::to_vec(&intent.next_chrome).ok()
        });
    if session_complete && chrome_complete {
        if let Err(error) = remove_file_durable(&intent_path) {
            eprintln!("[grokptah] committed session recovery intent cleanup deferred: {error:#}");
        }
        return Ok(());
    }
    rollback_session_creation(&intent)?;
    remove_file_durable(&intent_path)
}

/// Migrate monolithic v1 workspace.json → v2 layout.
fn migrate_v1_if_needed() -> Result<()> {
    let path = chrome_path();
    if !path.is_file() {
        return Ok(());
    }
    // Already migrated if sessions/ has content or chrome parses as v2 without sessions key.
    let raw = fs::read_to_string(&path)?;
    // v1 shape: { version, sessions: [...] }
    #[derive(Deserialize)]
    struct V1 {
        #[serde(default)]
        version: u32,
        project_cwd: Option<String>,
        active_session: Option<Uuid>,
        #[serde(default)]
        open_tab_ids: Vec<Uuid>,
        #[serde(default)]
        model: String,
        #[serde(default)]
        effort: EffortLevel,
        #[serde(default)]
        sandbox_profile: String,
        #[serde(default)]
        appearance: String,
        #[serde(default)]
        always_approve: bool,
        #[serde(default)]
        sessions: Vec<Session>,
    }
    let Ok(v1) = serde_json::from_str::<V1>(&raw) else {
        return Ok(());
    };
    if v1.sessions.is_empty() {
        // Might already be chrome-only (v2) — re-save clean chrome if version < 2
        if v1.version < STORE_VERSION && !raw.contains("\"sessions\"") {
            return Ok(());
        }
        if v1.sessions.is_empty() && v1.version >= STORE_VERSION {
            return Ok(());
        }
    }
    if v1.sessions.is_empty() {
        // chrome-only file that still has version 1
        let chrome = WorkspaceChrome {
            version: STORE_VERSION,
            project_cwd: v1.project_cwd,
            active_session: v1.active_session,
            open_tab_ids: v1.open_tab_ids,
            model: if v1.model.is_empty() {
                crate::models_catalog::resolve_default_model()
            } else {
                v1.model
            },
            effort: v1.effort,
            sandbox_profile: if v1.sandbox_profile.is_empty() {
                "workspace-write".into()
            } else {
                v1.sandbox_profile
            },
            appearance: if v1.appearance.is_empty() {
                "dark".into()
            } else {
                v1.appearance
            },
            always_approve: v1.always_approve,
            subagent_isolation: SubagentIsolationPreference::Worktree,
        };
        save_chrome(&chrome)?;
        return Ok(());
    }

    eprintln!(
        "[grokptah] migrating {} sessions from monolithic workspace.json → per-session store",
        v1.sessions.len()
    );
    for mut s in v1.sessions {
        s.transcript_loaded = true;
        s.persisted_len = 0;
        rewrite_transcript(&s)?;
    }
    let chrome = WorkspaceChrome {
        version: STORE_VERSION,
        project_cwd: v1.project_cwd,
        active_session: v1.active_session,
        open_tab_ids: v1.open_tab_ids,
        model: if v1.model.is_empty() {
            crate::models_catalog::resolve_default_model()
        } else {
            v1.model
        },
        effort: v1.effort,
        sandbox_profile: if v1.sandbox_profile.is_empty() {
            "workspace-write".into()
        } else {
            v1.sandbox_profile
        },
        appearance: if v1.appearance.is_empty() {
            "dark".into()
        } else {
            v1.appearance
        },
        always_approve: v1.always_approve,
        subagent_isolation: SubagentIsolationPreference::Worktree,
    };
    // Backup then replace
    let bak = path.with_extension("json.v1.bak");
    let _ = fs::copy(&path, &bak);
    save_chrome(&chrome)?;
    eprintln!(
        "[grokptah] migration complete (backup at {})",
        bak.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};
    use crate::session::Session;
    use std::io::Write;

    #[test]
    fn load_skips_torn_trailing_jsonl_line() {
        let _g = home_override_serial();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        set_grokptah_home_override(Some(home.clone()));

        let id = Uuid::new_v4();
        let dir = home.join("sessions").join(id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let tpath = dir.join("transcript.jsonl");
        {
            let mut f = std::fs::File::create(&tpath).unwrap();
            let good = serde_json::to_string(&TranscriptEntry::user("hello")).unwrap();
            writeln!(f, "{good}").unwrap();
            write!(f, "{{\"role\":\"assistant\",\"text\":\"torn").unwrap();
            f.flush().unwrap();
        }

        let mut session = Session::new(tmp.path().to_path_buf(), "m".into(), EffortLevel::Medium);
        session.id = id;
        session.transcript_loaded = false;
        load_transcript(&mut session).expect("load must succeed");
        assert_eq!(session.transcript.len(), 1);
        assert_eq!(session.transcript[0].role, "user");
        assert_eq!(session.transcript[0].text, "hello");

        set_grokptah_home_override(None);
    }

    #[test]
    fn append_writes_complete_reloadable_lines() {
        let _g = home_override_serial();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        set_grokptah_home_override(Some(home));

        let mut session = Session::new(tmp.path().to_path_buf(), "m".into(), EffortLevel::Medium);
        session.transcript.push(TranscriptEntry::user("a"));
        session.transcript.push(TranscriptEntry::assistant("b"));
        session.persisted_len = 0;
        append_transcript(&session, 0).unwrap();
        session.transcript.clear();
        session.transcript_loaded = false;
        load_transcript(&mut session).unwrap();
        assert_eq!(session.transcript.len(), 2);

        set_grokptah_home_override(None);
    }

    #[test]
    fn new_session_rolls_back_at_each_durable_boundary() {
        for boundary in ["transcript", "meta", "chrome"] {
            let _g = home_override_serial();
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join(".grokptah");
            std::fs::create_dir_all(home.join("sessions")).unwrap();
            set_grokptah_home_override(Some(home));

            let prior = WorkspaceChrome::default();
            save_chrome(&prior).unwrap();
            let before = std::fs::read(chrome_path()).unwrap();
            let session = Session::new(tmp.path().to_path_buf(), "m".into(), EffortLevel::Medium);
            let next = WorkspaceChrome {
                active_session: Some(session.id),
                open_tab_ids: vec![session.id],
                ..prior
            };

            set_test_persistence_failure(Some(boundary));
            assert!(create_session_durable(&session, &next).is_err());
            set_test_persistence_failure(None);

            assert!(
                !session_dir(session.id).exists(),
                "{boundary} left a session"
            );
            assert_eq!(std::fs::read(chrome_path()).unwrap(), before);
            assert!(!grokptah_home().join(SESSION_CREATION_INTENT_FILE).exists());
            set_grokptah_home_override(None);
        }
    }

    #[test]
    fn restart_recovery_removes_phantom_session_but_keeps_committed_session() {
        let _g = home_override_serial();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        set_grokptah_home_override(Some(home));
        let prior = WorkspaceChrome::default();
        save_chrome(&prior).unwrap();
        let prior_bytes = std::fs::read(chrome_path()).unwrap();

        let phantom = Session::new(tmp.path().to_path_buf(), "m".into(), EffortLevel::Medium);
        let phantom_next = WorkspaceChrome {
            active_session: Some(phantom.id),
            open_tab_ids: vec![phantom.id],
            ..prior.clone()
        };
        let phantom_intent = SessionCreationIntent {
            session_id: phantom.id,
            prior_chrome: Some(prior_bytes.clone()),
            next_chrome: phantom_next,
        };
        atomic_write_json(
            &grokptah_home().join(SESSION_CREATION_INTENT_FILE),
            &phantom_intent,
        )
        .unwrap();
        rewrite_transcript(&phantom).unwrap();
        let (_, loaded) = load_workspace().unwrap();
        assert!(!loaded.contains_key(&phantom.id));
        assert!(!session_dir(phantom.id).exists());
        assert_eq!(std::fs::read(chrome_path()).unwrap(), prior_bytes);

        let committed = Session::new(tmp.path().to_path_buf(), "m".into(), EffortLevel::Medium);
        let committed_next = WorkspaceChrome {
            active_session: Some(committed.id),
            open_tab_ids: vec![committed.id],
            ..prior
        };
        rewrite_transcript(&committed).unwrap();
        save_chrome(&committed_next).unwrap();
        let committed_intent = SessionCreationIntent {
            session_id: committed.id,
            prior_chrome: Some(std::fs::read(chrome_path()).unwrap()),
            next_chrome: committed_next,
        };
        atomic_write_json(
            &grokptah_home().join(SESSION_CREATION_INTENT_FILE),
            &committed_intent,
        )
        .unwrap();
        let (_, loaded) = load_workspace().unwrap();
        assert!(loaded.contains_key(&committed.id));
        assert!(!grokptah_home().join(SESSION_CREATION_INTENT_FILE).exists());

        set_grokptah_home_override(None);
    }
}
