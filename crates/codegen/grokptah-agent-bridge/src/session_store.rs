//! Persist GrokPtah sessions + workspace chrome across app restarts.
//!
//! Layout: `~/.grokptah/workspace.json` (single atomic write).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::discover::{ensure_home, grokptah_home};
use crate::session::Session;
use crate::types::EffortLevel;

const STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub project_cwd: Option<String>,
    pub active_session: Option<Uuid>,
    /// Session tabs the UI had open last time (order preserved).
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
    pub sessions: Vec<Session>,
}

impl Default for WorkspaceSnapshot {
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
            sessions: Vec::new(),
        }
    }
}

pub fn store_path() -> PathBuf {
    grokptah_home().join("workspace.json")
}

pub fn load() -> Result<WorkspaceSnapshot> {
    let path = store_path();
    if !path.is_file() {
        return Ok(WorkspaceSnapshot::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut snap: WorkspaceSnapshot = serde_json::from_str(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    snap.version = STORE_VERSION;
    // Drop sessions whose cwd vanished (optional soft keep — keep them;
    // user may reconnect the drive later).
    Ok(snap)
}

pub fn save(snap: &WorkspaceSnapshot) -> Result<()> {
    ensure_home();
    let path = store_path();
    let tmp = path.with_extension("json.tmp");
    let raw = serde_json::to_string_pretty(snap).context("serialize workspace")?;
    fs::write(&tmp, raw).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

/// Build a HashMap for the host from a loaded snapshot.
pub fn sessions_map(snap: &WorkspaceSnapshot) -> HashMap<Uuid, Session> {
    snap.sessions
        .iter()
        .cloned()
        .map(|s| (s.id, s))
        .collect()
}

/// Whether a path still looks like a usable project root.
pub fn cwd_still_valid(cwd: Option<&str>) -> Option<PathBuf> {
    let s = cwd?;
    let p = Path::new(s);
    if p.is_dir() {
        Some(p.to_path_buf())
    } else {
        None
    }
}
