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

use crate::host_runtime::DurableWriteGuard;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::discover::{ensure_home, grokptah_home};
use crate::orchestration::RunExecutionMode;
use crate::session::{Session, TranscriptEntry};
use crate::types::{EffortLevel, SubagentIsolationPreference};

const STORE_VERSION: u32 = 2;

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
pub fn save_prompt_queue(
    _write: &DurableWriteGuard,
    id: Uuid,
    queue: &crate::prompt_queue::SessionPromptQueue,
) -> Result<()> {
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
pub fn save_session_subagents(
    _write: &DurableWriteGuard,
    id: Uuid,
    list: &[crate::types::SubagentInfo],
) -> Result<()> {
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
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&filtered)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
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
/// Loading can migrate a v1 workspace in place, which is a durable write —
/// so it demands write authority even though the common path only reads.
pub fn load_workspace(
    write: &DurableWriteGuard,
) -> Result<(WorkspaceChrome, HashMap<Uuid, Session>)> {
    ensure_home();
    let _ = fs::create_dir_all(sessions_root());

    // One-shot migration from monolithic v1 file.
    migrate_v1_if_needed(write)?;

    let chrome = load_chrome().unwrap_or_default();
    let sessions = load_all_metas()?;
    Ok((chrome, sessions))
}

pub fn save_chrome(_write: &DurableWriteGuard, chrome: &WorkspaceChrome) -> Result<()> {
    ensure_home();
    let mut c = chrome.clone();
    c.version = STORE_VERSION;
    atomic_write_json(&chrome_path(), &c)
}

pub fn save_session_meta(_write: &DurableWriteGuard, session: &Session) -> Result<()> {
    let dir = session_dir(session.id);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let meta = SessionMeta::from_session(session);
    atomic_write_json(&meta_path(session.id), &meta)
}

/// Append transcript entries that are not yet on disk (`from_index..`).
/// Returns how many lines were written.
///
/// Each entry is serialized to a complete line string first, then written as a
/// single `write_all` so a crash cannot leave a half-encoded JSON object
/// mid-line (#118).
pub fn append_transcript(
    write: &DurableWriteGuard,
    session: &Session,
    from_index: usize,
) -> Result<usize> {
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
    // Durability on turn boundary (best-effort on platforms without full fsync).
    let _ = f.sync_all();
    // Keep meta.message_count in sync
    save_session_meta(write, session)?;
    Ok(n)
}

/// Full rewrite of transcript.jsonl (rewind / fork only — never compact).
/// Compact must not call this: local history is append-only forever.
pub fn rewrite_transcript(write: &DurableWriteGuard, session: &Session) -> Result<()> {
    let dir = session_dir(session.id);
    fs::create_dir_all(&dir)?;
    let path = transcript_path(session.id);
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        let mut batch = String::new();
        for entry in &session.transcript {
            let line = serde_json::to_string(entry)?;
            batch.push_str(&line);
            batch.push('\n');
        }
        f.write_all(batch.as_bytes())?;
        f.flush()?;
        let _ = f.sync_all();
    }
    // Sync parent dir so the rename is durable.
    if let Some(parent) = path.parent() {
        if let Ok(dirf) = File::open(parent) {
            let _ = dirf.sync_all();
        }
    }
    fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    if let Some(parent) = path.parent() {
        if let Ok(dirf) = File::open(parent) {
            let _ = dirf.sync_all();
        }
    }
    save_session_meta(write, session)?;
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
pub fn delete_session(_write: &DurableWriteGuard, id: Uuid) -> Result<()> {
    let dir = session_dir(id);
    if dir.is_dir() {
        fs::remove_dir_all(&dir).with_context(|| format!("rm -rf {}", dir.display()))?;
    }
    Ok(())
}

/// Soft GC: drop empty "New session" shells older than `max_age` and cap
/// **active** (non-archived) session dirs. Never deletes open tabs or archived.
pub fn garbage_collect(
    write: &DurableWriteGuard,
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
            delete_session(write, m.id)?;
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
            delete_session(write, m.id)?;
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

// ── Atomic JSON replacement (#216) ──────────────────────────────────────────
//
// Every write stages into its own exclusively created file next to the
// destination, then renames it over the destination. Concurrent writers used
// to share `<name>.json.tmp`: one writer's `File::create` truncated the file
// another writer had already staged (tearing the published snapshot), and the
// loser's rename then failed with ENOENT because the winner had moved the
// shared file away.

/// Fresh staging names tried before giving up. A v4 UUID collision is not
/// expected in practice; the bound only stops a pathological directory from
/// looping forever.
const STAGING_ATTEMPTS: usize = 8;

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    // Encode before touching the filesystem: a value that cannot be serialized
    // fails without creating a staging file or disturbing the destination.
    let raw = serde_json::to_string_pretty(value)
        .with_context(|| format!("serialize {}", path.display()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let (tmp, file) = create_exclusive_staging(path)?;
    stage_and_replace(path, &tmp, file, raw.as_bytes())
        .map_err(|error| discard_own_staging(&tmp, error))
}

/// Create a staging file that no other writer can hold: a fresh name in the
/// destination's directory (so the rename stays on one filesystem), opened
/// with `create_new` so an existing file is never opened, truncated or reused.
fn create_exclusive_staging(path: &Path) -> Result<(PathBuf, File)> {
    let name = path
        .file_name()
        .with_context(|| format!("atomic write target has no file name: {}", path.display()))?;
    for _ in 0..STAGING_ATTEMPTS {
        let mut staged = OsString::from(".");
        staged.push(name);
        staged.push(".tmp-");
        staged.push(staging_suffix());
        let tmp = path.with_file_name(staged);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            // Somebody else's file: leave it alone and pick another name.
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).with_context(|| format!("create {}", tmp.display())),
        }
    }
    bail!(
        "no free staging name for {} after {STAGING_ATTEMPTS} attempts",
        path.display()
    )
}

#[cfg(not(test))]
fn staging_suffix() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
fn staging_suffix() -> String {
    staging_seam::next_forced_suffix().unwrap_or_else(|| Uuid::new_v4().to_string())
}

/// Write the snapshot into this operation's own staging file and rename it
/// over `path`. The file is closed before the rename. Durability is
/// best-effort exactly as before: file and directory syncs are attempted, and
/// platforms without full fsync still get an atomic replacement.
fn stage_and_replace(path: &Path, tmp: &Path, mut file: File, bytes: &[u8]) -> Result<()> {
    #[cfg(test)]
    staging_seam::hit(staging_seam::Stage::BeforeWrite, tmp)
        .with_context(|| format!("write {}", tmp.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", tmp.display()))?;
    file.flush()
        .with_context(|| format!("write {}", tmp.display()))?;
    let _ = file.sync_all();
    drop(file);
    sync_parent_best_effort(path);
    #[cfg(test)]
    staging_seam::hit(staging_seam::Stage::BeforeRename, tmp)
        .with_context(|| format!("rename {}", path.display()))?;
    fs::rename(tmp, path).with_context(|| format!("rename {}", path.display()))?;
    sync_parent_best_effort(path);
    Ok(())
}

fn sync_parent_best_effort(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

/// Remove the staging file this failed operation created, and nothing else.
/// The original failure stays the error's source. A cleanup failure is added
/// as context that repeats the original message first, so callers that print
/// only the outermost message (`{error}`) still see what actually failed.
fn discard_own_staging(tmp: &Path, error: anyhow::Error) -> anyhow::Error {
    match fs::remove_file(tmp) {
        Ok(()) => error,
        Err(cleanup) if cleanup.kind() == ErrorKind::NotFound => error,
        Err(cleanup) => {
            let note = format!(
                "{error}; also could not remove staging file {}: {cleanup}",
                tmp.display()
            );
            error.context(note)
        }
    }
}

/// Test-only fault and interleaving points inside [`atomic_write_json`].
///
/// Hooks are thread-local: a hook installed by one test thread can never
/// observe or disturb a write made on any other thread.
#[cfg(test)]
mod staging_seam {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::Path;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Stage {
        /// The staging file exists and is still empty.
        BeforeWrite,
        /// The staging file holds the complete snapshot; nothing is renamed yet.
        BeforeRename,
    }

    type Hook = Box<dyn FnMut(Stage, &Path) -> std::io::Result<()>>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static SUFFIXES: RefCell<VecDeque<String>> = const { RefCell::new(VecDeque::new()) };
    }

    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
            SUFFIXES.with(|slot| slot.borrow_mut().clear());
        }
    }

    /// Run `body` on this thread with `hook` called at each [`Stage`], and
    /// with the first staging names drawn from `suffixes`. Both are cleared
    /// afterwards, including on panic.
    pub(super) fn with<R>(
        suffixes: &[&str],
        hook: impl FnMut(Stage, &Path) -> std::io::Result<()> + 'static,
        body: impl FnOnce() -> R,
    ) -> R {
        let _reset = Reset;
        let forced: VecDeque<String> = suffixes.iter().map(|s| (*s).to_owned()).collect();
        SUFFIXES.with(|slot| *slot.borrow_mut() = forced);
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        body()
    }

    pub(super) fn hit(stage: Stage, staging: &Path) -> std::io::Result<()> {
        // Take the hook out while it runs so a re-entrant write sees no hook
        // instead of a RefCell borrow panic.
        let mut hook = HOOK.with(|slot| slot.borrow_mut().take());
        let result = hook.as_mut().map_or(Ok(()), |hook| hook(stage, staging));
        HOOK.with(|slot| *slot.borrow_mut() = hook);
        result
    }

    pub(super) fn next_forced_suffix() -> Option<String> {
        SUFFIXES.with(|slot| slot.borrow_mut().pop_front())
    }
}

/// Migrate monolithic v1 workspace.json → v2 layout.
fn migrate_v1_if_needed(write: &DurableWriteGuard) -> Result<()> {
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
        save_chrome(write, &chrome)?;
        return Ok(());
    }

    eprintln!(
        "[grokptah] migrating {} sessions from monolithic workspace.json → per-session store",
        v1.sessions.len()
    );
    for mut s in v1.sessions {
        s.transcript_loaded = true;
        s.persisted_len = 0;
        rewrite_transcript(write, &s)?;
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
    save_chrome(write, &chrome)?;
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
    use crate::prompt_queue::SessionPromptQueue as Queue;
    use crate::session::Session;
    use std::io::Write;
    use std::sync::mpsc;

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
        append_transcript(
            &crate::host_runtime::DurableWriteGuard::unowned_for_test(),
            &session,
            0,
        )
        .unwrap();
        session.transcript.clear();
        session.transcript_loaded = false;
        load_transcript(&mut session).unwrap();
        assert_eq!(session.transcript.len(), 2);

        set_grokptah_home_override(None);
    }

    // ── #216: atomic JSON replacement ───────────────────────────────────────

    /// Staging files planted by [`plant_foreign_staging`], sorted.
    const FOREIGN_NAMES: [&str; 2] = [".workspace.json.tmp-foreign", "workspace.json.tmp"];

    /// An isolated project home installed as the process-wide override for one
    /// test. Dropping it restores the override before the temporary directory
    /// is removed and releases the serial guard last. Tests join every worker
    /// they start before this drops, so no writer can land in a home that is
    /// being torn down.
    struct IsolatedHome {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl IsolatedHome {
        fn new() -> Self {
            let serial = home_override_serial();
            let tmp = tempfile::tempdir().expect("temporary project home");
            let home = tmp.path().join(".grokptah");
            fs::create_dir_all(home.join("sessions")).expect("sessions root");
            set_grokptah_home_override(Some(home.clone()));
            Self {
                _tmp: tmp,
                home,
                _serial: serial,
            }
        }
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            set_grokptah_home_override(None);
        }
    }

    fn write_authority() -> DurableWriteGuard {
        DurableWriteGuard::unowned_for_test()
    }

    /// Explicit chrome: `WorkspaceChrome::default()` would consult the real
    /// user's model configuration.
    fn chrome(project: &str, model: &str) -> WorkspaceChrome {
        WorkspaceChrome {
            version: STORE_VERSION,
            project_cwd: Some(project.to_owned()),
            active_session: None,
            open_tab_ids: Vec::new(),
            model: model.to_owned(),
            effort: EffortLevel::High,
            sandbox_profile: "workspace-write".into(),
            appearance: "dark".into(),
            always_approve: false,
            subagent_isolation: SubagentIsolationPreference::Worktree,
        }
    }

    /// The exact bytes `save_chrome` publishes for `chrome`.
    fn snapshot(chrome: &WorkspaceChrome) -> Vec<u8> {
        serde_json::to_vec_pretty(chrome).expect("encode chrome")
    }

    fn read_bytes(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    /// Every staging-shaped name in `dir`: the old shared form or the new
    /// private one.
    fn staging_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("list directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        names.sort();
        names
    }

    /// Staging files that belong to other writers: one under the old shared
    /// name and one in the new private form.
    fn plant_foreign_staging(dest: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let name = dest.file_name().expect("destination name");
        let legacy = dest.with_extension("json.tmp");
        let private = dest.with_file_name(format!(".{}.tmp-foreign", name.to_string_lossy()));
        let foreign = vec![
            (legacy, b"legacy writer".to_vec()),
            (private, b"other writer".to_vec()),
        ];
        for (path, bytes) in &foreign {
            fs::write(path, bytes).expect("plant foreign staging file");
        }
        foreign
    }

    fn assert_foreign_staging_intact(foreign: &[(PathBuf, Vec<u8>)]) {
        for (path, bytes) in foreign {
            assert_eq!(read_bytes(path), *bytes, "{} changed", path.display());
        }
    }

    /// #216 parent-red, through the real write/rename path with no seam.
    ///
    /// Writer B has staged a complete snapshot under `workspace.json.tmp`, the
    /// one name the previous helper used for every writer, and has not renamed
    /// it yet. Writer A then saves, and B finishes with its own rename. The
    /// previous helper's `File::create` reopened B's file, replaced its bytes
    /// with A's and renamed it away, so B's rename failed with ENOENT: the
    /// `rename … ENOENT` symptom reported in #216.
    #[test]
    fn save_chrome_leaves_another_writers_staged_snapshot_alone() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        let shared = dest.with_extension("json.tmp");
        let a = chrome("/work/a", "model-a");
        let b = chrome("/work/b", "model-b");
        fs::write(&shared, snapshot(&b)).expect("writer B stages its snapshot");

        save_chrome(&write_authority(), &a).expect("writer A saves");
        assert_eq!(read_bytes(&dest), snapshot(&a));

        fs::rename(&shared, &dest).expect("writer B's rename still finds its own file");
        assert_eq!(
            read_bytes(&dest),
            snapshot(&b),
            "writer A truncated or replaced writer B's staged snapshot"
        );
        let (reopened, _) = load_workspace(&write_authority()).expect("reopen");
        assert_eq!(snapshot(&reopened), snapshot(&b));
        assert!(staging_names(&home.home).is_empty());
    }

    /// #216 parent-red, through the real write/rename path with no seam.
    ///
    /// Writer B has opened `workspace.json.tmp`, as the previous helper's
    /// `File::create` did, but has not written yet. Writer A then saves a long
    /// snapshot, and B writes its short one through the handle it still holds.
    /// The previous helper had published the very file B held open, so B's
    /// bytes landed on top of A's published chrome and left torn JSON, which
    /// reopen silently replaced with default chrome.
    #[test]
    fn a_writer_holding_the_old_shared_staging_name_cannot_tear_published_chrome() {
        let _home = IsolatedHome::new();
        let dest = chrome_path();
        let shared = dest.with_extension("json.tmp");
        let a = chrome("/work/a-project-whose-path-is-much-longer", "model-a");
        let b = chrome("/b", "b");
        assert!(snapshot(&b).len() < snapshot(&a).len());
        let mut writer_b = File::create(shared).expect("writer B opens its staging file");

        save_chrome(&write_authority(), &a).expect("writer A saves");
        writer_b
            .write_all(&snapshot(&b))
            .expect("writer B writes through its handle");
        drop(writer_b);

        assert_eq!(
            read_bytes(&dest),
            snapshot(&a),
            "another writer's bytes reached the published workspace.json"
        );
        let (reopened, _) = load_workspace(&write_authority()).expect("reopen");
        assert_eq!(snapshot(&reopened), snapshot(&a));
    }

    /// Two real `save_chrome` writers, interleaved deterministically through
    /// the staging seam: A stages its complete snapshot and is parked just
    /// before its rename while B runs a whole save. Each stages privately,
    /// neither disturbs the other, both succeed, and the published file is
    /// always one complete submitted snapshot. The survivor is whichever
    /// rename commits last, which is A here although A started first.
    #[test]
    fn interleaved_save_chrome_writers_stage_privately_and_both_commit() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        let a = chrome("/work/a", "model-a");
        let b = chrome("/work/b-with-a-longer-path", "model-b");

        std::thread::scope(|scope| {
            let (a_staged_tx, a_staged_rx) = mpsc::channel::<PathBuf>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let a_ref = &a;
            let writer_a = scope.spawn(move || {
                staging_seam::with(
                    &[],
                    move |stage, staging| {
                        if stage == staging_seam::Stage::BeforeRename {
                            let _ = a_staged_tx.send(staging.to_path_buf());
                            // Parked until released, or until the test unwinds
                            // and drops the sender.
                            let _ = release_rx.recv();
                        }
                        Ok(())
                    },
                    || save_chrome(&write_authority(), a_ref),
                )
            });
            let a_staging = a_staged_rx.recv().expect("writer A reaches its rename");
            assert_eq!(read_bytes(&a_staging), snapshot(&a));

            let (b_staged_tx, b_staged_rx) = mpsc::channel::<PathBuf>();
            let b_ref = &b;
            let writer_b = scope.spawn(move || {
                staging_seam::with(
                    &[],
                    move |stage, staging| {
                        if stage == staging_seam::Stage::BeforeRename {
                            let _ = b_staged_tx.send(staging.to_path_buf());
                        }
                        Ok(())
                    },
                    || save_chrome(&write_authority(), b_ref),
                )
            });
            writer_b
                .join()
                .expect("writer B thread")
                .expect("writer B saves while A is parked");
            let b_staging = b_staged_rx.recv().expect("writer B staged");

            assert_ne!(a_staging, b_staging, "writers shared a staging file");
            assert_eq!(a_staging.parent(), dest.parent());
            assert_eq!(b_staging.parent(), dest.parent());
            assert_eq!(
                read_bytes(&a_staging),
                snapshot(&a),
                "writer B truncated or replaced writer A's staged snapshot"
            );
            assert!(!b_staging.exists());
            assert_eq!(read_bytes(&dest), snapshot(&b));

            release_tx.send(()).expect("release writer A");
            writer_a
                .join()
                .expect("writer A thread")
                .expect("writer A's rename finds its own staging file");
            assert!(!a_staging.exists());
            assert_eq!(read_bytes(&dest), snapshot(&a));
        });

        assert!(staging_names(&home.home).is_empty());
        let (reopened, _) = load_workspace(&write_authority()).expect("reopen");
        assert_eq!(snapshot(&reopened), snapshot(&a));
    }

    /// Unscripted concurrency on top of the deterministic case: every writer
    /// succeeds and, whatever order the renames commit in, the published file
    /// is one complete submitted snapshot. No ordering is assumed.
    #[test]
    fn concurrent_save_chrome_writers_all_succeed_and_publish_a_submitted_snapshot() {
        const WRITERS: usize = 4;
        const ROUNDS: usize = 8;
        let home = IsolatedHome::new();
        let submitted: Vec<WorkspaceChrome> = (0..WRITERS)
            .map(|n| chrome(&format!("/work/writer-{n}"), &format!("model-{n}")))
            .collect();
        let gate = std::sync::RwLock::new(());

        std::thread::scope(|scope| {
            // Held until every writer is spawned so they start together. If a
            // spawn panics, unwinding drops it and the writers already started
            // still finish, so the scope's join cannot hang.
            let hold = gate.write().expect("fresh start gate");
            let writers: Vec<_> = submitted
                .iter()
                .map(|mine| {
                    let gate = &gate;
                    scope.spawn(move || {
                        let _started = gate.read();
                        for _ in 0..ROUNDS {
                            save_chrome(&write_authority(), mine)?;
                        }
                        Ok::<(), anyhow::Error>(())
                    })
                })
                .collect();
            drop(hold);
            for writer in writers {
                writer
                    .join()
                    .expect("writer thread")
                    .expect("every concurrent save succeeds");
            }
        });

        let published = read_bytes(&chrome_path());
        assert!(
            submitted.iter().any(|c| snapshot(c) == published),
            "published chrome is not one complete submitted snapshot"
        );
        assert!(staging_names(&home.home).is_empty());
        let (reopened, _) = load_workspace(&write_authority()).expect("reopen");
        assert_eq!(snapshot(&reopened), published);
    }

    #[test]
    fn save_chrome_overwrite_replaces_the_whole_previous_snapshot() {
        let home = IsolatedHome::new();
        let long = chrome("/work/a-long-project-path-that-is-replaced", "model-long");
        let short = chrome("/s", "m");
        save_chrome(&write_authority(), &long).expect("first save");
        save_chrome(&write_authority(), &short).expect("overwrite");

        assert_eq!(read_bytes(&chrome_path()), snapshot(&short));
        assert!(staging_names(&home.home).is_empty());
        let (reopened, _) = load_workspace(&write_authority()).expect("reopen");
        assert_eq!(snapshot(&reopened), snapshot(&short));
    }

    struct Unencodable;

    impl Serialize for Unencodable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("injected serialization failure"))
        }
    }

    /// A value that cannot be encoded fails before any file is created: the
    /// destination and every other writer's staging file stay exactly as they
    /// were, and the encoder's own error reaches the caller.
    #[test]
    fn serialization_failure_creates_nothing_and_preserves_the_destination() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        let a = chrome("/work/a", "model-a");
        save_chrome(&write_authority(), &a).expect("prior snapshot");
        let foreign = plant_foreign_staging(&dest);

        let error = atomic_write_json(&dest, &Unencodable).expect_err("encoding must fail");
        let msg = format!("{error:#}");
        assert!(msg.contains("injected serialization failure"), "{msg}");
        assert_eq!(read_bytes(&dest), snapshot(&a));
        assert_eq!(staging_names(&home.home), FOREIGN_NAMES);
        assert_foreign_staging_intact(&foreign);
    }

    /// A failed write is reported with its cause, removes only the staging
    /// file this write created, and leaves the published snapshot and every
    /// other writer's staging file untouched.
    #[test]
    fn write_failure_is_reported_and_removes_only_its_own_staging_file() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        let a = chrome("/work/a", "model-a");
        save_chrome(&write_authority(), &a).expect("prior snapshot");
        let foreign = plant_foreign_staging(&dest);

        let (staged_tx, staged_rx) = mpsc::channel::<PathBuf>();
        let result = staging_seam::with(
            &[],
            move |stage, staging| {
                if stage != staging_seam::Stage::BeforeWrite {
                    return Ok(());
                }
                let _ = staged_tx.send(staging.to_path_buf());
                Err(std::io::Error::other("injected write failure"))
            },
            || save_chrome(&write_authority(), &chrome("/work/b", "model-b")),
        );
        let error = result.expect_err("the write failure must be reported");

        let own = staged_rx
            .recv()
            .expect("the write reached its staging file");
        assert_eq!(error.root_cause().to_string(), "injected write failure");
        let msg = format!("{error:#}");
        let write = format!("write {}", own.display());
        assert!(msg.contains(&write), "{msg}");
        assert!(!own.exists(), "own staging file left behind");
        assert_eq!(read_bytes(&dest), snapshot(&a));
        assert_eq!(staging_names(&home.home), FOREIGN_NAMES);
        assert_foreign_staging_intact(&foreign);
    }

    /// A rename the OS refuses (a directory occupies the destination) is
    /// reported with the OS error as its cause, removes only this write's
    /// staging file, and leaves the occupant and every other writer's staging
    /// file untouched. No seam fault: the failure is real.
    #[test]
    fn real_rename_failure_is_reported_and_removes_only_its_own_staging_file() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        fs::create_dir(&dest).expect("occupy the destination with a directory");
        fs::write(dest.join("occupant"), b"unrelated").expect("occupant content");
        let foreign = plant_foreign_staging(&dest);

        let (staged_tx, staged_rx) = mpsc::channel::<PathBuf>();
        let result = staging_seam::with(
            &[],
            move |stage, staging| {
                if stage == staging_seam::Stage::BeforeRename {
                    let _ = staged_tx.send(staging.to_path_buf());
                }
                Ok(())
            },
            || save_chrome(&write_authority(), &chrome("/work/a", "model-a")),
        );
        let error = result.expect_err("an unreplaceable destination must fail the save");

        let own = staged_rx.recv().expect("the write staged its snapshot");
        let msg = format!("{error:#}");
        let rename = format!("rename {}", dest.display());
        assert!(msg.contains(&rename), "{msg}");
        let cause = error.root_cause();
        assert!(cause.is::<std::io::Error>(), "{msg}");
        assert!(!own.exists(), "own staging file left behind");
        assert!(dest.is_dir());
        assert_eq!(read_bytes(&dest.join("occupant")), b"unrelated");
        assert_eq!(staging_names(&home.home), FOREIGN_NAMES);
        assert_foreign_staging_intact(&foreign);
    }

    /// If even the cleanup fails, the original failure stays the error's root
    /// cause and the cleanup failure is added beside it. Cleanup only unlinks
    /// a file: it never removes a directory tree.
    #[test]
    fn cleanup_failure_keeps_the_original_error_and_never_removes_a_tree() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        let a = chrome("/work/a", "model-a");
        save_chrome(&write_authority(), &a).expect("prior snapshot");

        let (staged_tx, staged_rx) = mpsc::channel::<PathBuf>();
        let result = staging_seam::with(
            &[],
            move |stage, staging| {
                if stage != staging_seam::Stage::BeforeRename {
                    return Ok(());
                }
                // Swap the staged file for a directory so removing it fails.
                fs::remove_file(staging)?;
                fs::create_dir(staging)?;
                fs::write(staging.join("occupant"), b"x")?;
                let _ = staged_tx.send(staging.to_path_buf());
                Err(std::io::Error::other("injected rename failure"))
            },
            || save_chrome(&write_authority(), &chrome("/work/b", "model-b")),
        );
        let error = result.expect_err("the rename failure must be reported");

        let own = staged_rx
            .recv()
            .expect("the seam replaced the staging file");
        assert_eq!(error.root_cause().to_string(), "injected rename failure");
        // Callers that print only the outermost message still see the
        // original failure first, then the cleanup failure.
        let top = error.to_string();
        let rename = format!("rename {}", dest.display());
        assert!(top.starts_with(&rename), "{top}");
        assert!(top.contains("could not remove staging"), "{top}");
        assert!(own.join("occupant").is_file(), "cleanup removed a tree");
        assert_eq!(read_bytes(&dest), snapshot(&a));
        assert_eq!(staging_names(&home.home).len(), 1);
    }

    /// A staging name that already exists belongs to someone else: it is
    /// skipped without being opened, truncated or removed.
    #[test]
    fn an_existing_staging_name_is_skipped_untouched() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        let taken = dest.with_file_name(".workspace.json.tmp-taken");
        fs::write(&taken, b"someone else's staging").expect("plant a taken name");
        let a = chrome("/work/a", "model-a");

        let save = || save_chrome(&write_authority(), &a);
        let result = staging_seam::with(&["taken"], |_, _| Ok(()), save);
        result.expect("the save picks a fresh name");

        assert_eq!(read_bytes(&taken), b"someone else's staging");
        assert_eq!(read_bytes(&dest), snapshot(&a));
        assert_eq!(staging_names(&home.home), [".workspace.json.tmp-taken"]);
    }

    /// When every candidate name is taken the save fails without opening,
    /// changing or removing any of them, and the destination is unchanged.
    #[test]
    fn exhausted_staging_names_fail_without_touching_anything() {
        let home = IsolatedHome::new();
        let dest = chrome_path();
        let a = chrome("/work/a", "model-a");
        save_chrome(&write_authority(), &a).expect("prior snapshot");
        let suffixes: Vec<String> = (0..STAGING_ATTEMPTS)
            .map(|n| format!("taken-{n}"))
            .collect();
        for suffix in &suffixes {
            let taken = dest.with_file_name(format!(".workspace.json.tmp-{suffix}"));
            fs::write(taken, suffix).expect("plant a taken name");
        }

        let forced: Vec<&str> = suffixes.iter().map(String::as_str).collect();
        let save = || save_chrome(&write_authority(), &chrome("/work/b", "model-b"));
        let result = staging_seam::with(&forced, |_, _| Ok(()), save);
        let error = result.expect_err("no staging name is free");

        let msg = format!("{error:#}");
        assert!(msg.contains("no free staging name"), "{msg}");
        assert_eq!(read_bytes(&dest), snapshot(&a));
        assert_eq!(staging_names(&home.home).len(), STAGING_ATTEMPTS);
        for suffix in &suffixes {
            let taken = dest.with_file_name(format!(".workspace.json.tmp-{suffix}"));
            assert_eq!(read_bytes(&taken), suffix.as_bytes());
        }
    }

    /// The one-shot v1 migration still splits a monolithic workspace into the
    /// per-session layout, keeps a byte-exact backup, and rewrites chrome
    /// through the helper without leaving staging files behind.
    #[test]
    fn v1_migration_still_splits_sessions_and_rewrites_chrome() {
        let home = IsolatedHome::new();
        let mut session = Session::new("/work/v1".into(), "model-v1".into(), EffortLevel::Medium);
        session.title = "Migrated".into();
        session.transcript.push(TranscriptEntry::user("question"));
        session
            .transcript
            .push(TranscriptEntry::assistant("answer"));
        let id = session.id;
        let v1 = serde_json::json!({
            "version": 1,
            "project_cwd": "/work/v1",
            "active_session": id,
            "open_tab_ids": [id],
            "model": "model-v1",
            "effort": "high",
            "sandbox_profile": "read-only",
            "appearance": "light",
            "always_approve": true,
            "sessions": [session]
        });
        let original = serde_json::to_vec_pretty(&v1).expect("encode v1 workspace");
        fs::write(chrome_path(), &original).expect("write v1 workspace");

        let (migrated, mut sessions) = load_workspace(&write_authority()).expect("migrate v1");
        assert_eq!(migrated.version, STORE_VERSION);
        assert_eq!(migrated.project_cwd.as_deref(), Some("/work/v1"));
        assert_eq!(migrated.active_session, Some(id));
        assert_eq!(migrated.open_tab_ids, [id]);
        assert_eq!(migrated.model, "model-v1");
        assert_eq!(migrated.effort, EffortLevel::High);
        assert_eq!(migrated.sandbox_profile, "read-only");
        assert_eq!(migrated.appearance, "light");
        assert!(migrated.always_approve);
        let raw = fs::read_to_string(chrome_path()).expect("migrated chrome");
        assert!(!raw.contains("\"sessions\""), "{raw}");
        let backup = chrome_path().with_extension("json.v1.bak");
        assert_eq!(read_bytes(&backup), original);

        let mut shell = sessions.remove(&id).expect("migrated session shell");
        assert_eq!(shell.title, "Migrated");
        assert_eq!(shell.persisted_len, 2);
        load_transcript(&mut shell).expect("load migrated transcript");
        assert_eq!(shell.transcript.len(), 2);
        assert_eq!(shell.transcript[1].text, "answer");
        assert!(staging_names(&home.home).is_empty());
        assert!(staging_names(&session_dir(id)).is_empty());
    }

    /// The prompt queue and session metadata still round-trip through the
    /// helper, overwrite cleanly, and leave no staging files behind.
    #[test]
    fn prompt_queue_and_session_meta_still_round_trip() {
        let _home = IsolatedHome::new();
        let write = write_authority();
        let mut session = Session::new("/work/q".into(), "model-q".into(), EffortLevel::Low);
        session.title = "Queued".into();
        save_session_meta(&write, &session).expect("save meta");

        let mut queue = Queue::default();
        let first = queue.add("first", "composer", false).unwrap();
        let urgent = queue.add("urgent", "composer", true).unwrap();
        save_prompt_queue(&write, session.id, &queue).expect("save queue");
        let ids: Vec<String> = load_prompt_queue(session.id)
            .expect("load queue")
            .list()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(ids, [urgent.id, first.id]);

        let mut shorter = Queue::default();
        let only = shorter.add("only", "composer", false).unwrap();
        save_prompt_queue(&write, session.id, &shorter).expect("overwrite queue");
        let reloaded = load_prompt_queue(session.id).unwrap().list();
        assert_eq!(reloaded, [only]);

        let metas = load_all_metas().expect("load metas");
        assert_eq!(metas[&session.id].title, "Queued");
        assert!(staging_names(&session_dir(session.id)).is_empty());
    }

    /// #216 changes only how a write is staged: the durable writers that use
    /// the helper still demand a `DurableWriteGuard`. This fails to compile
    /// if that requirement is dropped from any of them.
    #[test]
    fn durable_writers_still_demand_write_authority() {
        let _: fn(&DurableWriteGuard, &WorkspaceChrome) -> Result<()> = save_chrome;
        let _: fn(&DurableWriteGuard, &Session) -> Result<()> = save_session_meta;
        let _: fn(&DurableWriteGuard, Uuid, &Queue) -> Result<()> = save_prompt_queue;
    }
}
