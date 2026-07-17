//! Project-scoped memory for Build sessions (thin port of memory flush/recall).
//!
//! Stores facts under `~/.grokptah/memory/<project-hash>.json` so a new session
//! on the same project cwd can load prior decisions.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discover::grokptah_home;

const MAX_FACTS: usize = 80;
const MAX_FACT_CHARS: usize = 800;
const MAX_INJECT_CHARS: usize = 6_000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectMemory {
    pub project_key: String,
    pub cwd: String,
    pub facts: Vec<MemoryFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: String,
    pub text: String,
    pub tags: Vec<String>,
    pub updated_at: String,
}

fn memory_dir() -> PathBuf {
    let d = grokptah_home().join("memory");
    let _ = fs::create_dir_all(&d);
    d
}

pub fn project_key(cwd: &Path) -> String {
    let canon = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let s = canon.display().to_string();
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn path_for(cwd: &Path) -> PathBuf {
    memory_dir().join(format!("{}.json", project_key(cwd)))
}

pub fn load(cwd: &Path) -> ProjectMemory {
    let path = path_for(cwd);
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(m) = serde_json::from_str::<ProjectMemory>(&raw) {
            return m;
        }
    }
    ProjectMemory {
        project_key: project_key(cwd),
        cwd: cwd.display().to_string(),
        facts: Vec::new(),
    }
}

pub fn save(cwd: &Path, mem: &ProjectMemory) -> anyhow::Result<()> {
    let path = path_for(cwd);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(mem)?;
    fs::write(path, raw)?;
    Ok(())
}

/// Append or update a fact. Returns the fact id.
pub fn remember(cwd: &Path, text: &str, tags: &[String]) -> anyhow::Result<String> {
    let text = text.trim();
    if text.is_empty() {
        anyhow::bail!("empty memory fact");
    }
    let text: String = text.chars().take(MAX_FACT_CHARS).collect();
    let mut mem = load(cwd);
    // Dedupe exact text
    if let Some(existing) = mem.facts.iter().find(|f| f.text == text) {
        return Ok(existing.id.clone());
    }
    let id = uuid::Uuid::new_v4().to_string();
    mem.facts.push(MemoryFact {
        id: id.clone(),
        text,
        tags: tags.to_vec(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    });
    if mem.facts.len() > MAX_FACTS {
        let drop_n = mem.facts.len() - MAX_FACTS;
        mem.facts.drain(0..drop_n);
    }
    mem.project_key = project_key(cwd);
    mem.cwd = cwd.display().to_string();
    save(cwd, &mem)?;
    Ok(id)
}

pub fn list_facts(cwd: &Path) -> Vec<MemoryFact> {
    load(cwd).facts
}

/// Search facts by substring (case-insensitive).
pub fn search(cwd: &Path, query: &str) -> Vec<MemoryFact> {
    let q = query.trim().to_ascii_lowercase();
    let mem = load(cwd);
    if q.is_empty() {
        return mem.facts;
    }
    mem.facts
        .into_iter()
        .filter(|f| {
            f.text.to_ascii_lowercase().contains(&q)
                || f.tags.iter().any(|t| t.to_ascii_lowercase().contains(&q))
        })
        .collect()
}

/// Text for Build system context injection.
pub fn inject_context(cwd: &Path) -> String {
    let mem = load(cwd);
    if mem.facts.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Project memory (facts from prior sessions on this project; honor unless user overrides):\n",
    );
    let mut used = out.len();
    for f in mem.facts.iter().rev() {
        let line = format!("- {}\n", f.text);
        if used + line.len() > MAX_INJECT_CHARS {
            break;
        }
        out.push_str(&line);
        used += line.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};

    #[test]
    fn remember_and_recall_across_load() {
        let _serial = home_override_serial();
        let home = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().to_path_buf()));
        let proj = tempfile::tempdir().unwrap();
        let id = remember(proj.path(), "Always use tabs in this repo", &[]).unwrap();
        assert!(!id.is_empty());
        let facts = list_facts(proj.path());
        assert_eq!(facts.len(), 1);
        assert!(facts[0].text.contains("tabs"));
        let inject = inject_context(proj.path());
        assert!(inject.contains("tabs"));
        set_grokptah_home_override(None);
    }
}
