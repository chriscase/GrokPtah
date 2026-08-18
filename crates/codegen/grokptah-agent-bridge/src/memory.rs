//! Durable, source-workspace-scoped memory for Build sessions.
//!
//! Project facts retain the legacy `~/.grokptah/memory/<project-hash>.json`
//! location. Agent-private and team facts live below a sibling scoped tree.
//! An execution worktree is deliberately absent from [`MemoryAccess`]: every
//! caller must provide the durable source workspace and an explicit scope.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

use crate::discover::grokptah_home;
use crate::orchestration::safe_id_filename;

const MAX_FACTS: usize = 80;
const MAX_FACT_CHARS: usize = 800;
const MAX_INJECT_CHARS: usize = 6_000;

/// Durable namespace selected by a memory caller.
///
/// Agent and team identifiers are part of the address, not fact metadata, so
/// facts cannot be returned from a broader scope by a filtering mistake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryScope {
    Project,
    AgentPrivate { agent_id: String },
    Team { team_id: String },
}

impl MemoryScope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::AgentPrivate { .. } => "agent_private",
            Self::Team { .. } => "team",
        }
    }
}

/// Caller identity and sharing policy bound to one durable source workspace.
///
/// The default policy has no approved teams. This lets team scope exist in the
/// storage contract now without granting sharing before manager/delegation
/// policy has made an explicit decision.
#[derive(Debug, Clone)]
pub struct MemoryAccess {
    source_workspace: PathBuf,
    actor_agent_id: Option<String>,
    approved_team_ids: HashSet<String>,
}

impl MemoryAccess {
    pub fn new(source_workspace: impl AsRef<Path>, actor_agent_id: Option<String>) -> Self {
        let requested = source_workspace.as_ref();
        let source_workspace =
            dunce::canonicalize(requested).unwrap_or_else(|_| requested.to_path_buf());
        Self {
            source_workspace,
            actor_agent_id,
            approved_team_ids: HashSet::new(),
        }
    }

    /// Add a team granted by the caller's already-evaluated sharing policy.
    pub fn allow_team(mut self, team_id: impl Into<String>) -> anyhow::Result<Self> {
        let team_id = team_id.into();
        validate_scope_id(&team_id, "team_id")?;
        self.approved_team_ids.insert(team_id);
        Ok(self)
    }

    pub fn source_workspace(&self) -> &Path {
        &self.source_workspace
    }

    pub fn actor_agent_id(&self) -> Option<&str> {
        self.actor_agent_id.as_deref()
    }

    /// Resolve and authorize the exact durable address selected by a caller.
    pub fn resolve(&self, scope: MemoryScope) -> anyhow::Result<MemoryAddress> {
        match &scope {
            MemoryScope::Project => {}
            MemoryScope::AgentPrivate { agent_id } => {
                validate_scope_id(agent_id, "agent_id")?;
                if self.actor_agent_id.as_deref() != Some(agent_id.as_str()) {
                    bail!("agent-private memory scope is not owned by the current agent");
                }
            }
            MemoryScope::Team { team_id } => {
                validate_scope_id(team_id, "team_id")?;
                if !self.approved_team_ids.contains(team_id) {
                    bail!("team memory scope is not approved by policy");
                }
            }
        }
        Ok(MemoryAddress {
            source_workspace: self.source_workspace.clone(),
            scope,
        })
    }

    pub fn project(&self) -> MemoryAddress {
        MemoryAddress {
            source_workspace: self.source_workspace.clone(),
            scope: MemoryScope::Project,
        }
    }
}

/// Fully resolved durable memory address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAddress {
    source_workspace: PathBuf,
    scope: MemoryScope,
}

impl MemoryAddress {
    pub fn source_workspace(&self) -> &Path {
        &self.source_workspace
    }

    pub fn scope(&self) -> &MemoryScope {
        &self.scope
    }
}

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

fn validate_scope_id(id: &str, field: &str) -> anyhow::Result<String> {
    safe_id_filename(id).map_err(|error| anyhow!("invalid {field}: {}", error.message))
}

fn memory_dir() -> PathBuf {
    let d = grokptah_home().join("memory");
    let _ = fs::create_dir_all(&d);
    d
}

pub fn project_key(source_workspace: &Path) -> String {
    let canon =
        dunce::canonicalize(source_workspace).unwrap_or_else(|_| source_workspace.to_path_buf());
    let s = canon.display().to_string();
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn path_for(address: &MemoryAddress) -> anyhow::Result<PathBuf> {
    let project_key = project_key(address.source_workspace());
    match address.scope() {
        // Compatibility contract: legacy project files remain canonical and
        // require no data rewrite or one-time migration.
        MemoryScope::Project => Ok(memory_dir().join(format!("{project_key}.json"))),
        MemoryScope::AgentPrivate { agent_id } => Ok(memory_dir()
            .join("scopes")
            .join(project_key)
            .join("agents")
            .join(format!("{}.json", validate_scope_id(agent_id, "agent_id")?))),
        MemoryScope::Team { team_id } => Ok(memory_dir()
            .join("scopes")
            .join(project_key)
            .join("teams")
            .join(format!("{}.json", validate_scope_id(team_id, "team_id")?))),
    }
}

pub fn load(address: &MemoryAddress) -> ProjectMemory {
    if let Ok(path) = path_for(address) {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(memory) = serde_json::from_str::<ProjectMemory>(&raw) {
                return memory;
            }
        }
    }
    ProjectMemory {
        project_key: project_key(address.source_workspace()),
        cwd: address.source_workspace().display().to_string(),
        facts: Vec::new(),
    }
}

pub fn save(address: &MemoryAddress, memory: &ProjectMemory) -> anyhow::Result<()> {
    let path = path_for(address)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(memory)?;
    fs::write(&path, raw).with_context(|| format!("write memory scope {}", path.display()))?;
    Ok(())
}

/// Append or update a fact. Returns the fact id.
pub fn remember(address: &MemoryAddress, text: &str, tags: &[String]) -> anyhow::Result<String> {
    let text = text.trim();
    if text.is_empty() {
        bail!("empty memory fact");
    }
    let text: String = text.chars().take(MAX_FACT_CHARS).collect();
    let mut memory = load(address);
    // Dedupe exact text.
    if let Some(existing) = memory.facts.iter().find(|fact| fact.text == text) {
        return Ok(existing.id.clone());
    }
    let id = uuid::Uuid::new_v4().to_string();
    memory.facts.push(MemoryFact {
        id: id.clone(),
        text,
        tags: tags.to_vec(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    });
    if memory.facts.len() > MAX_FACTS {
        let drop_n = memory.facts.len() - MAX_FACTS;
        memory.facts.drain(0..drop_n);
    }
    memory.project_key = project_key(address.source_workspace());
    memory.cwd = address.source_workspace().display().to_string();
    save(address, &memory)?;
    Ok(id)
}

pub fn list_facts(address: &MemoryAddress) -> Vec<MemoryFact> {
    load(address).facts
}

/// Search facts by substring (case-insensitive).
pub fn search(address: &MemoryAddress, query: &str) -> Vec<MemoryFact> {
    let query = query.trim().to_ascii_lowercase();
    let memory = load(address);
    if query.is_empty() {
        return memory.facts;
    }
    memory
        .facts
        .into_iter()
        .filter(|fact| {
            fact.text.to_ascii_lowercase().contains(&query)
                || fact
                    .tags
                    .iter()
                    .any(|tag| tag.to_ascii_lowercase().contains(&query))
        })
        .collect()
}

/// Text for Build system context injection. Content and bounds intentionally
/// match the legacy project-memory implementation.
pub fn inject_context(address: &MemoryAddress) -> String {
    let memory = load(address);
    if memory.facts.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Project memory (facts from prior sessions on this project; honor unless user overrides):\n",
    );
    let mut used = out.len();
    for fact in memory.facts.iter().rev() {
        let line = format!("- {}\n", fact.text);
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

    struct IsolatedHome {
        _serial: std::sync::MutexGuard<'static, ()>,
        _home: tempfile::TempDir,
    }

    impl IsolatedHome {
        fn install() -> Self {
            let serial = home_override_serial();
            let home = tempfile::tempdir().unwrap();
            set_grokptah_home_override(Some(home.path().to_path_buf()));
            Self {
                _serial: serial,
                _home: home,
            }
        }
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            set_grokptah_home_override(None);
        }
    }

    #[test]
    fn legacy_project_file_is_reused_across_access_and_restart() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let access = MemoryAccess::new(source.path(), None);
        let address = access.resolve(MemoryScope::Project).unwrap();
        let id = remember(&address, "Always use tabs in this repo", &[]).unwrap();
        assert!(!id.is_empty());

        let legacy_path = memory_dir().join(format!("{}.json", project_key(source.path())));
        assert!(
            legacy_path.is_file(),
            "project facts stay at the legacy path"
        );

        let restarted = MemoryAccess::new(source.path(), None)
            .resolve(MemoryScope::Project)
            .unwrap();
        let facts = list_facts(&restarted);
        assert_eq!(facts.len(), 1);
        assert!(inject_context(&restarted).contains("tabs"));
    }

    #[test]
    fn isolated_execution_directory_never_changes_project_address() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let isolated_execution = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(source.path(), None).project();

        remember(&address, "shared from an isolated run", &[]).unwrap();
        assert_ne!(
            project_key(source.path()),
            project_key(isolated_execution.path())
        );
        assert_eq!(list_facts(&address)[0].text, "shared from an isolated run");
        assert!(!memory_dir()
            .join(format!("{}.json", project_key(isolated_execution.path())))
            .exists());
    }

    #[test]
    fn private_agent_scopes_do_not_cross() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let first = MemoryAccess::new(source.path(), Some("agent-one".into()));
        let second = MemoryAccess::new(source.path(), Some("agent-two".into()));
        let first_address = first
            .resolve(MemoryScope::AgentPrivate {
                agent_id: "agent-one".into(),
            })
            .unwrap();
        remember(&first_address, "first agent only", &[]).unwrap();

        assert!(second
            .resolve(MemoryScope::AgentPrivate {
                agent_id: "agent-one".into(),
            })
            .is_err());
        let second_address = second
            .resolve(MemoryScope::AgentPrivate {
                agent_id: "agent-two".into(),
            })
            .unwrap();
        assert!(list_facts(&second_address).is_empty());
    }

    #[test]
    fn team_scope_requires_explicit_policy_approval() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let scope = MemoryScope::Team {
            team_id: "release-team".into(),
        };
        let denied = MemoryAccess::new(source.path(), Some("agent-one".into()));
        assert!(denied.resolve(scope.clone()).is_err());

        let approved = MemoryAccess::new(source.path(), Some("agent-one".into()))
            .allow_team("release-team")
            .unwrap();
        let address = approved.resolve(scope).unwrap();
        remember(&address, "approved shared ritual", &[]).unwrap();
        assert_eq!(list_facts(&address).len(), 1);
    }

    #[test]
    fn memory_survives_execution_promotion_or_discard_cleanup() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let promoted_execution = tempfile::tempdir().unwrap();
        let discarded_execution = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(source.path(), None).project();

        remember(&address, "written before promotion", &[]).unwrap();
        fs::write(promoted_execution.path().join("change.txt"), "promoted").unwrap();
        fs::write(source.path().join("change.txt"), "promoted").unwrap();
        drop(promoted_execution);

        remember(&address, "written before discard", &[]).unwrap();
        drop(discarded_execution);

        let texts: Vec<_> = list_facts(&address)
            .into_iter()
            .map(|fact| fact.text)
            .collect();
        assert_eq!(
            texts,
            vec!["written before promotion", "written before discard"]
        );
    }
}
