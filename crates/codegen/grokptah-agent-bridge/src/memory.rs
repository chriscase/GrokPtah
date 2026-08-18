//! Durable, source-workspace-scoped memory for Build sessions.
//!
//! Project facts retain the legacy `~/.grokptah/memory/<project-hash>.json`
//! location. Agent-private and team facts live below a sibling scoped tree
//! keyed by a versioned SHA-256 digest of the canonical source workspace. An
//! execution worktree is deliberately absent from [`MemoryAccess`].

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use anyhow::{anyhow, bail, Context};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::discover::grokptah_home;
use crate::orchestration::safe_id_filename;

const MAX_FACTS: usize = 80;
const MAX_FACT_CHARS: usize = 800;
const MAX_INJECT_CHARS: usize = 6_000;
const SCOPED_WORKSPACE_KEY_VERSION: &str = "v1-sha256";

/// Process-local serialization for each exact durable memory address. Weak
/// entries keep the registry from retaining one mutex forever per old scope.
static ADDRESS_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Durable namespace selected by an adapter. Authorization remains owned by
/// [`crate::host::AgentHost`]; this DTO cannot resolve or access storage.
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

/// Host-owned identity and sharing policy bound to one durable source
/// workspace. This capability is intentionally not exported from the crate.
#[derive(Debug, Clone)]
pub(crate) struct MemoryAccess {
    source_workspace: PathBuf,
    actor_agent_id: Option<String>,
    approved_team_ids: HashSet<String>,
}

impl MemoryAccess {
    pub(crate) fn new(source_workspace: impl AsRef<Path>, actor_agent_id: Option<String>) -> Self {
        Self {
            source_workspace: canonical_workspace(source_workspace.as_ref()),
            actor_agent_id,
            approved_team_ids: HashSet::new(),
        }
    }

    /// Test/future policy seam. Production callers cannot self-approve a team;
    /// an AgentHost policy will own this when team membership ships.
    #[cfg(test)]
    fn allow_team(mut self, team_id: impl Into<String>) -> anyhow::Result<Self> {
        let team_id = team_id.into();
        validate_scope_id(&team_id, "team_id")?;
        self.approved_team_ids.insert(team_id);
        Ok(self)
    }

    pub(crate) fn actor_agent_id(&self) -> Option<&str> {
        self.actor_agent_id.as_deref()
    }

    /// Resolve and authorize the exact durable address selected by a caller.
    pub(crate) fn resolve(&self, scope: MemoryScope) -> anyhow::Result<MemoryAddress> {
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

    pub(crate) fn project(&self) -> MemoryAddress {
        MemoryAddress {
            source_workspace: self.source_workspace.clone(),
            scope: MemoryScope::Project,
        }
    }
}

/// Fully authorized durable memory address. Construction and storage access
/// stay inside the runtime crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryAddress {
    source_workspace: PathBuf,
    scope: MemoryScope,
}

impl MemoryAddress {
    pub(crate) fn source_workspace(&self) -> &Path {
        &self.source_workspace
    }

    pub(crate) fn scope(&self) -> &MemoryScope {
        &self.scope
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectMemory {
    project_key: String,
    cwd: String,
    facts: Vec<MemoryFact>,
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
    grokptah_home().join("memory")
}

fn canonical_workspace(source_workspace: &Path) -> PathBuf {
    dunce::canonicalize(source_workspace).unwrap_or_else(|_| source_workspace.to_path_buf())
}

/// Exact legacy key algorithm and filename contract for project memory.
fn legacy_project_key(source_workspace: &Path) -> String {
    let canonical = canonical_workspace(source_workspace);
    let mut hasher = DefaultHasher::new();
    canonical.display().to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn scoped_workspace_key(source_workspace: &Path) -> String {
    let canonical = canonical_workspace(source_workspace);
    let digest = Sha256::digest(canonical.display().to_string().as_bytes());
    format!("{SCOPED_WORKSPACE_KEY_VERSION}-{:x}", digest)
}

fn storage_key(address: &MemoryAddress) -> String {
    match address.scope() {
        MemoryScope::Project => legacy_project_key(address.source_workspace()),
        MemoryScope::AgentPrivate { .. } | MemoryScope::Team { .. } => {
            scoped_workspace_key(address.source_workspace())
        }
    }
}

fn path_for(address: &MemoryAddress) -> anyhow::Result<PathBuf> {
    match address.scope() {
        // Compatibility contract: legacy project files remain canonical and
        // require no data rewrite or one-time migration.
        MemoryScope::Project => Ok(memory_dir().join(format!(
            "{}.json",
            legacy_project_key(address.source_workspace())
        ))),
        MemoryScope::AgentPrivate { agent_id } => Ok(memory_dir()
            .join("scopes")
            .join(scoped_workspace_key(address.source_workspace()))
            .join("agents")
            .join(format!("{}.json", validate_scope_id(agent_id, "agent_id")?))),
        MemoryScope::Team { team_id } => Ok(memory_dir()
            .join("scopes")
            .join(scoped_workspace_key(address.source_workspace()))
            .join("teams")
            .join(format!("{}.json", validate_scope_id(team_id, "team_id")?))),
    }
}

fn address_lock(path: &Path) -> Arc<Mutex<()>> {
    let mut locks = ADDRESS_LOCKS.lock();
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

fn empty_memory(address: &MemoryAddress) -> ProjectMemory {
    ProjectMemory {
        project_key: storage_key(address),
        cwd: address.source_workspace().display().to_string(),
        facts: Vec::new(),
    }
}

fn verify_workspace_identity(
    address: &MemoryAddress,
    path: &Path,
    memory: &ProjectMemory,
) -> anyhow::Result<()> {
    let expected_workspace = canonical_workspace(address.source_workspace());
    let stored_workspace = Path::new(&memory.cwd);
    let stored_canonical = dunce::canonicalize(stored_workspace).unwrap_or_else(|_| {
        if stored_workspace == expected_workspace {
            expected_workspace.clone()
        } else {
            stored_workspace.to_path_buf()
        }
    });
    if stored_canonical != expected_workspace {
        bail!(
            "memory workspace mismatch at {}: requested {}, stored {}",
            path.display(),
            expected_workspace.display(),
            stored_workspace.display()
        );
    }
    let expected_key = storage_key(address);
    if memory.project_key != expected_key {
        bail!(
            "memory workspace key mismatch at {}: requested {}, stored {}",
            path.display(),
            expected_key,
            memory.project_key
        );
    }
    Ok(())
}

fn load_from_path(address: &MemoryAddress, path: &Path) -> anyhow::Result<Option<ProjectMemory>> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read memory scope {}", path.display()))
        }
    };
    let memory: ProjectMemory = serde_json::from_str(&raw)
        .with_context(|| format!("parse memory scope {}", path.display()))?;
    verify_workspace_identity(address, path, &memory)?;
    Ok(Some(memory))
}

fn load(address: &MemoryAddress) -> anyhow::Result<ProjectMemory> {
    let path = path_for(address)?;
    Ok(load_from_path(address, &path)?.unwrap_or_else(|| empty_memory(address)))
}

fn atomic_write(path: &Path, raw: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("memory path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memory.json");
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut temp = options
            .open(&temp_path)
            .with_context(|| format!("create memory temp file {}", temp_path.display()))?;
        temp.write_all(raw)
            .with_context(|| format!("write memory temp file {}", temp_path.display()))?;
        temp.flush()
            .with_context(|| format!("flush memory temp file {}", temp_path.display()))?;
        temp.sync_all()
            .with_context(|| format!("sync memory temp file {}", temp_path.display()))?;
        drop(temp);
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "atomically replace memory scope {} from {}",
                path.display(),
                temp_path.display()
            )
        })?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn save_to_path(
    address: &MemoryAddress,
    path: &Path,
    memory: &ProjectMemory,
) -> anyhow::Result<()> {
    verify_workspace_identity(address, path, memory)?;
    let raw = serde_json::to_vec_pretty(memory)?;
    atomic_write(path, &raw)
}

/// Append or update a fact. The complete read-modify-write transaction is
/// serialized for this address, so concurrent Lanes cannot overwrite one
/// another's updates.
pub(crate) fn remember(
    address: &MemoryAddress,
    text: &str,
    tags: &[String],
) -> anyhow::Result<String> {
    let text = text.trim();
    if text.is_empty() {
        bail!("empty memory fact");
    }
    let text: String = text.chars().take(MAX_FACT_CHARS).collect();
    let path = path_for(address)?;
    let lock = address_lock(&path);
    let _guard = lock.lock();
    let mut memory = load_from_path(address, &path)?.unwrap_or_else(|| empty_memory(address));
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
    memory.project_key = storage_key(address);
    memory.cwd = address.source_workspace().display().to_string();
    save_to_path(address, &path, &memory)?;
    Ok(id)
}

pub(crate) fn list_facts(address: &MemoryAddress) -> anyhow::Result<Vec<MemoryFact>> {
    Ok(load(address)?.facts)
}

/// Search facts by substring (case-insensitive).
pub(crate) fn search(address: &MemoryAddress, query: &str) -> anyhow::Result<Vec<MemoryFact>> {
    let query = query.trim().to_ascii_lowercase();
    let memory = load(address)?;
    if query.is_empty() {
        return Ok(memory.facts);
    }
    Ok(memory
        .facts
        .into_iter()
        .filter(|fact| {
            fact.text.to_ascii_lowercase().contains(&query)
                || fact
                    .tags
                    .iter()
                    .any(|tag| tag.to_ascii_lowercase().contains(&query))
        })
        .collect())
}

/// Text for Build system context injection. Content and bounds intentionally
/// match the legacy project-memory implementation.
pub(crate) fn inject_context(address: &MemoryAddress) -> anyhow::Result<String> {
    let memory = load(address)?;
    if memory.facts.is_empty() {
        return Ok(String::new());
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
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};
    use std::sync::Barrier;

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

        let legacy_path = memory_dir().join(format!("{}.json", legacy_project_key(source.path())));
        assert!(
            legacy_path.is_file(),
            "project facts stay at the legacy path"
        );

        let restarted = MemoryAccess::new(source.path(), None)
            .resolve(MemoryScope::Project)
            .unwrap();
        let facts = list_facts(&restarted).unwrap();
        assert_eq!(facts.len(), 1);
        assert!(inject_context(&restarted).unwrap().contains("tabs"));
    }

    #[test]
    fn isolated_execution_directory_never_changes_project_address() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let isolated_execution = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(source.path(), None).project();

        remember(&address, "shared from an isolated run", &[]).unwrap();
        assert_ne!(
            legacy_project_key(source.path()),
            legacy_project_key(isolated_execution.path())
        );
        assert_eq!(
            list_facts(&address).unwrap()[0].text,
            "shared from an isolated run"
        );
        assert!(!memory_dir()
            .join(format!(
                "{}.json",
                legacy_project_key(isolated_execution.path())
            ))
            .exists());
    }

    #[test]
    fn private_agent_scopes_do_not_cross_and_use_versioned_sha_workspace_keys() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let first = MemoryAccess::new(source.path(), Some("agent-one".into()));
        let second = MemoryAccess::new(source.path(), Some("agent-two".into()));
        let first_address = first
            .resolve(MemoryScope::AgentPrivate {
                agent_id: "agent-one".into(),
            })
            .unwrap();
        let first_path = path_for(&first_address).unwrap();
        let scoped_key = scoped_workspace_key(source.path());
        assert!(first_path
            .components()
            .any(|part| part.as_os_str() == std::ffi::OsStr::new(&scoped_key)));
        assert!(!first_path
            .to_string_lossy()
            .contains(&legacy_project_key(source.path())));
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
        assert!(list_facts(&second_address).unwrap().is_empty());
    }

    #[test]
    fn team_scope_requires_runtime_policy_approval() {
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
        assert_eq!(list_facts(&address).unwrap().len(), 1);
    }

    #[test]
    fn concurrent_same_scope_writes_preserve_both_lane_facts() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(source.path(), None).project();
        let start = Arc::new(Barrier::new(3));
        let mut writers = Vec::new();
        for lane in ["lane-one", "lane-two"] {
            let address = address.clone();
            let start = start.clone();
            writers.push(std::thread::spawn(move || {
                start.wait();
                remember(&address, lane, &[]).unwrap();
            }));
        }
        start.wait();
        for writer in writers {
            writer.join().unwrap();
        }
        let mut facts: Vec<_> = list_facts(&address)
            .unwrap()
            .into_iter()
            .map(|fact| fact.text)
            .collect();
        facts.sort();
        assert_eq!(facts, vec!["lane-one", "lane-two"]);
    }

    #[test]
    fn corrupt_canonical_file_is_never_treated_as_empty_or_overwritten() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(source.path(), None).project();
        remember(&address, "valid fact", &[]).unwrap();
        let path = path_for(&address).unwrap();
        fs::write(&path, b"{ truncated").unwrap();

        assert!(list_facts(&address).is_err());
        assert!(remember(&address, "must not erase history", &[]).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{ truncated");
    }

    #[cfg(unix)]
    #[test]
    fn durable_memory_files_are_private_to_the_runtime_user() {
        use std::os::unix::fs::PermissionsExt;

        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(source.path(), Some("private-agent".into()))
            .resolve(MemoryScope::AgentPrivate {
                agent_id: "private-agent".into(),
            })
            .unwrap();
        remember(&address, "private fact", &[]).unwrap();

        let mode = fs::metadata(path_for(&address).unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn interrupted_sibling_temp_never_replaces_canonical_memory() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(source.path(), None).project();
        remember(&address, "canonical fact", &[]).unwrap();
        let path = path_for(&address).unwrap();
        let sibling = path.with_extension("json.interrupted.tmp");
        fs::write(&sibling, b"partial").unwrap();

        let facts = list_facts(&address).unwrap();
        assert_eq!(facts[0].text, "canonical fact");
        remember(&address, "later fact", &[]).unwrap();
        assert_eq!(list_facts(&address).unwrap().len(), 2);
        assert_eq!(fs::read(&sibling).unwrap(), b"partial");
    }

    #[test]
    fn legacy_hash_collision_candidate_fails_closed_without_orphaning_valid_facts() {
        let _home = IsolatedHome::install();
        let requested = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let address = MemoryAccess::new(requested.path(), None).project();
        let path = path_for(&address).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let foreign = ProjectMemory {
            project_key: legacy_project_key(requested.path()),
            cwd: canonical_workspace(other.path()).display().to_string(),
            facts: vec![MemoryFact {
                id: "foreign".into(),
                text: "must not leak".into(),
                tags: vec![],
                updated_at: "now".into(),
            }],
        };
        let original = serde_json::to_vec_pretty(&foreign).unwrap();
        fs::write(&path, &original).unwrap();

        assert!(list_facts(&address).is_err());
        assert!(remember(&address, "must not overwrite", &[]).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
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
            .unwrap()
            .into_iter()
            .map(|fact| fact.text)
            .collect();
        assert_eq!(
            texts,
            vec!["written before promotion", "written before discard"]
        );
    }
}
