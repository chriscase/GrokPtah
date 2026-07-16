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

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::discover::{ensure_home, grokptah_home};
use crate::session::{Session, TranscriptEntry};
use crate::types::EffortLevel;

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
        }
    }
}

// ── Per-session metadata (no transcript body) ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: Uuid,
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
    pub compacted_summary: Option<String>,
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

// ── Public API ──────────────────────────────────────────────────────────────

/// Load chrome + session shells (transcripts empty until [`load_transcript`]).
pub fn load_workspace() -> Result<(WorkspaceChrome, HashMap<Uuid, Session>)> {
    ensure_home();
    let _ = fs::create_dir_all(sessions_root());

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

pub fn save_session_meta(session: &Session) -> Result<()> {
    let dir = session_dir(session.id);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let meta = SessionMeta::from_session(session);
    atomic_write_json(&meta_path(session.id), &meta)
}

/// Append transcript entries that are not yet on disk (`from_index..`).
/// Returns how many lines were written.
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
    for entry in session.transcript.iter().skip(from_index) {
        serde_json::to_writer(&mut f, entry)?;
        f.write_all(b"\n")?;
        n += 1;
    }
    f.flush()?;
    // Keep meta.message_count in sync
    save_session_meta(session)?;
    Ok(n)
}

/// Full rewrite of transcript.jsonl (rewind / compact).
pub fn rewrite_transcript(session: &Session) -> Result<()> {
    let dir = session_dir(session.id);
    fs::create_dir_all(&dir)?;
    let path = transcript_path(session.id);
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        for entry in &session.transcript {
            serde_json::to_writer(&mut f, entry)?;
            f.write_all(b"\n")?;
        }
        f.flush()?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    save_session_meta(session)?;
    Ok(())
}

/// Load full transcript into a session shell. No-op if already loaded.
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
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {} of {}", i + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: TranscriptEntry = serde_json::from_str(&line)
            .with_context(|| format!("parse line {} of {}", i + 1, path.display()))?;
        entries.push(entry);
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
            title: s.title.clone(),
            cwd: s.cwd.display().to_string(),
            created_at: s.created_at,
            updated_at: s.updated_at,
            forked_from: s.forked_from,
            model: s.model.clone(),
            effort: s.effort,
            plan_mode: s.plan_mode,
            plan_steps: s.plan_steps.clone(),
            compacted_summary: s.compacted_summary.clone(),
            message_count: s.transcript.len().max(s.persisted_len),
            folder: s.folder.clone(),
            tags: s.tags.clone(),
            archived: s.archived,
            archived_at: s.archived_at,
        }
    }

    fn into_shell(self) -> Session {
        Session {
            id: self.id,
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
            compacted_summary: self.compacted_summary,
            folder: self.folder,
            tags: self.tags,
            archived: self.archived,
            archived_at: self.archived_at,
            transcript_loaded: false,
            // Until load_transcript, treat disk as authoritative length.
            persisted_len: self.message_count,
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let raw = serde_json::to_string_pretty(value)?;
    fs::write(&tmp, raw).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
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
