//! Durable, source-workspace-scoped memory for Build sessions.
//!
//! Project facts retain the legacy `~/.grokptah/memory/<project-hash>.json`
//! location. Agent-private and team facts live below a sibling scoped tree
//! keyed by a versioned SHA-256 digest of the canonical source workspace. An
//! execution worktree is deliberately absent from [`MemoryAccess`].
//!
//! v2 is additive: legacy v1 `{id,text,tags,updated_at}` records deserialize
//! and remain meaningful. New writes carry revision, validity, supersession,
//! bounded criticality/salience, and attributable source metadata. Retention
//! is no longer naive FIFO: expired, then superseded, then lowest-priority
//! noncritical facts compact first. A current critical fact is never dropped
//! to admit an 81st write.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::discover::grokptah_home;
use crate::orchestration::safe_id_filename;

const MAX_FACTS: usize = 80;
const MAX_FACT_CHARS: usize = 800;
const MAX_TAGS: usize = 16;
const MAX_TAG_CHARS: usize = 64;
const MAX_ID_CHARS: usize = 128;
const MAX_KEY_CHARS: usize = 128;
const MAX_SOURCE_ACTOR_CHARS: usize = 128;
const MAX_HOT_STORE_BYTES: usize = 128 * 1024;
const MAX_INJECT_CHARS: usize = 6_000;
const SCHEMA_VERSION: u32 = 2;
const SCOPED_WORKSPACE_KEY_VERSION: &str = "v1-sha256";
const FACT_ID_PREFIX: &str = "m2-";

/// Process-local serialization for each exact durable memory address. Weak
/// entries keep the registry from retaining one mutex forever per old scope.
static ADDRESS_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Injected clock for every new timestamp and validity decision. Production
/// uses [`SystemClock`]; tests construct [`FakeClock`] at a fixed instant.
pub(crate) trait Clock: Send + Sync + fmt::Debug {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Deterministic clock. Construction sets the instant; tests advance or jump
/// it explicitly. There are no sleeps and no Tokio time claims.
#[derive(Debug)]
pub(crate) struct FakeClock {
    now: Mutex<DateTime<Utc>>,
}

impl FakeClock {
    pub(crate) fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set(&self, now: DateTime<Utc>) {
        *self.now.lock() = now;
    }

    #[allow(dead_code)]
    pub(crate) fn advance(&self, delta: Duration) {
        *self.now.lock() += delta;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock()
    }
}

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

    /// Declared v2 hot-store bounds. Certification compares the fixture oracle
    /// against these constants rather than re-deriving them.
    pub fn long_horizon_declared_bounds() -> serde_json::Value {
        serde_json::json!({
            "max_facts": MAX_FACTS,
            "max_fact_chars": MAX_FACT_CHARS,
            "max_tags": MAX_TAGS,
            "max_tag_chars": MAX_TAG_CHARS,
            "max_id_chars": MAX_ID_CHARS,
            "max_key_chars": MAX_KEY_CHARS,
            "max_source_actor_chars": MAX_SOURCE_ACTOR_CHARS,
            "max_hot_store_bytes": MAX_HOT_STORE_BYTES,
            "schema_version": SCHEMA_VERSION,
        })
    }

    /// Typed, bounded versioned write. `now_rfc3339` is the injected clock
    /// instant for this transaction.
    pub fn write_versioned(
        &self,
        source_workspace: impl AsRef<Path>,
        actor_agent_id: Option<&str>,
        approved_team_ids: &[&str],
        now_rfc3339: &str,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let address = authorized_address(
            self,
            source_workspace,
            actor_agent_id,
            approved_team_ids,
            now_rfc3339,
        )?;
        let request: VersionedWriteRequest =
            serde_json::from_value(request).map_err(|_| MemoryError::Malformed.json())?;
        remember_versioned(&address, request)
            .map(|ack| serde_json::to_value(ack).expect("acknowledgement is json"))
            .map_err(MemoryError::json)
    }

    /// Authoritative retrieval at a supplied clock instant.
    pub fn retrieve_at(
        &self,
        source_workspace: impl AsRef<Path>,
        actor_agent_id: Option<&str>,
        approved_team_ids: &[&str],
        at_rfc3339: &str,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let address = authorized_address(
            self,
            source_workspace,
            actor_agent_id,
            approved_team_ids,
            at_rfc3339,
        )?;
        let at = parse_timestamp(at_rfc3339).ok_or_else(|| MemoryError::Malformed.json())?;
        retrieve_at(&address, at)
            .map(|retrieval| retrieval.to_json())
            .map_err(MemoryError::json)
    }

    /// Legacy substring search (case-insensitive). Not semantic retrieval.
    pub fn search_legacy(
        &self,
        source_workspace: impl AsRef<Path>,
        actor_agent_id: Option<&str>,
        approved_team_ids: &[&str],
        now_rfc3339: &str,
        query: &str,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let address = authorized_address(
            self,
            source_workspace,
            actor_agent_id,
            approved_team_ids,
            now_rfc3339,
        )?;
        let facts = search(&address, query).map_err(|_| MemoryError::Durable.json())?;
        Ok(serde_json::json!({
            "facts": facts.iter().map(fact_view).collect::<Vec<_>>(),
        }))
    }

    /// Existing remember() behavior, timestamped from the injected clock.
    pub fn remember_compat(
        &self,
        source_workspace: impl AsRef<Path>,
        actor_agent_id: Option<&str>,
        approved_team_ids: &[&str],
        now_rfc3339: &str,
        text: &str,
        tags: &[String],
    ) -> Result<serde_json::Value, serde_json::Value> {
        let address = authorized_address(
            self,
            source_workspace,
            actor_agent_id,
            approved_team_ids,
            now_rfc3339,
        )?;
        remember(&address, text, tags)
            .map(|id| serde_json::json!({ "id": id }))
            .map_err(|error| classify_remember_error(&error).json())
    }

    /// Canonical durable file for this authorized scope.
    pub fn durable_path(
        &self,
        source_workspace: impl AsRef<Path>,
        actor_agent_id: Option<&str>,
        approved_team_ids: &[&str],
    ) -> Result<PathBuf, serde_json::Value> {
        let address = authorized_address(
            self,
            source_workspace,
            actor_agent_id,
            approved_team_ids,
            "2000-01-01T00:00:00.000Z",
        )?;
        path_for(&address).map_err(|_| MemoryError::Durable.json())
    }

    /// Deterministic fact id that a versioned write with `idempotency_key`
    /// would receive in this canonical scope.
    pub fn preview_versioned_id(
        &self,
        source_workspace: impl AsRef<Path>,
        actor_agent_id: Option<&str>,
        approved_team_ids: &[&str],
        idempotency_key: &str,
    ) -> Result<String, serde_json::Value> {
        let address = authorized_address(
            self,
            source_workspace,
            actor_agent_id,
            approved_team_ids,
            "2000-01-01T00:00:00.000Z",
        )?;
        Ok(fact_id_for(&address, idempotency_key))
    }

    /// Persisted hot-store byte length (0 when the canonical file is absent).
    pub fn hot_store_bytes(
        &self,
        source_workspace: impl AsRef<Path>,
        actor_agent_id: Option<&str>,
        approved_team_ids: &[&str],
    ) -> Result<usize, serde_json::Value> {
        let path = self.durable_path(source_workspace, actor_agent_id, approved_team_ids)?;
        match fs::read(&path) {
            Ok(raw) => Ok(raw.len()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
            Err(_) => Err(MemoryError::Durable.json()),
        }
    }
}

fn authorized_address(
    scope: &MemoryScope,
    source_workspace: impl AsRef<Path>,
    actor_agent_id: Option<&str>,
    approved_team_ids: &[&str],
    now_rfc3339: &str,
) -> Result<MemoryAddress, serde_json::Value> {
    let now = parse_timestamp(now_rfc3339).ok_or_else(|| MemoryError::Malformed.json())?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(now));
    let access = MemoryAccess::new(source_workspace, actor_agent_id.map(str::to_string))
        .with_clock(clock)
        .with_approved_teams(approved_team_ids.iter().map(|id| (*id).to_string()))
        .map_err(|_| MemoryError::Malformed.json())?;
    access
        .resolve(scope.clone())
        .map_err(|_| MemoryError::CrossScope.json())
}

/// Host-owned identity and sharing policy bound to one durable source
/// workspace. This capability is intentionally not exported from the crate.
#[derive(Debug, Clone)]
pub(crate) struct MemoryAccess {
    source_workspace: PathBuf,
    actor_agent_id: Option<String>,
    project_allowed: bool,
    agent_private_allowed: bool,
    approved_team_ids: HashSet<String>,
    clock: Arc<dyn Clock>,
}

impl MemoryAccess {
    pub(crate) fn new(source_workspace: impl AsRef<Path>, actor_agent_id: Option<String>) -> Self {
        Self {
            source_workspace: canonical_workspace(source_workspace.as_ref()),
            actor_agent_id,
            project_allowed: true,
            agent_private_allowed: true,
            approved_team_ids: HashSet::new(),
            clock: Arc::new(SystemClock),
        }
    }

    pub(crate) fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
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

    /// Narrow team capability captured from the durable Agent specification.
    /// Invalid IDs fail before any storage address is resolved.
    pub(crate) fn with_approved_teams(
        mut self,
        team_ids: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<Self> {
        for team_id in team_ids {
            validate_scope_id(&team_id, "team_id")?;
            self.approved_team_ids.insert(team_id);
        }
        Ok(self)
    }

    /// Apply the exact memory ceiling from a frozen Agent specification.
    pub(crate) fn with_agent_policy(
        mut self,
        project_allowed: bool,
        agent_private_allowed: bool,
        team_ids: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<Self> {
        self.project_allowed = project_allowed;
        self.agent_private_allowed = agent_private_allowed;
        self.approved_team_ids.clear();
        self.with_approved_teams(team_ids)
    }

    /// Resolve and authorize the exact durable address selected by a caller.
    pub(crate) fn resolve(&self, scope: MemoryScope) -> anyhow::Result<MemoryAddress> {
        match &scope {
            MemoryScope::Project if !self.project_allowed => {
                bail!("project memory scope is disabled by Agent policy");
            }
            MemoryScope::Project => {}
            MemoryScope::AgentPrivate { agent_id } => {
                validate_scope_id(agent_id, "agent_id")?;
                if !self.agent_private_allowed {
                    bail!("agent-private memory scope is disabled by Agent policy");
                }
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
        Ok(self.address(scope))
    }

    pub(crate) fn project(&self) -> MemoryAddress {
        self.address(MemoryScope::Project)
    }

    pub(crate) fn project_if_allowed(&self) -> Option<MemoryAddress> {
        self.project_allowed.then(|| self.project())
    }

    fn address(&self, scope: MemoryScope) -> MemoryAddress {
        MemoryAddress {
            source_workspace: self.source_workspace.clone(),
            scope,
            clock: self.clock.clone(),
            actor_agent_id: self.actor_agent_id.clone(),
        }
    }
}

/// Fully authorized durable memory address. Construction and storage access
/// stay inside the runtime crate.
#[derive(Clone)]
pub(crate) struct MemoryAddress {
    source_workspace: PathBuf,
    scope: MemoryScope,
    clock: Arc<dyn Clock>,
    actor_agent_id: Option<String>,
}

impl PartialEq for MemoryAddress {
    fn eq(&self, other: &Self) -> bool {
        self.source_workspace == other.source_workspace && self.scope == other.scope
    }
}

impl Eq for MemoryAddress {}

impl fmt::Debug for MemoryAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryAddress")
            .field("source_workspace", &self.source_workspace)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
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
    #[serde(default, skip_serializing_if = "schema_is_legacy")]
    schema_version: u32,
    facts: Vec<MemoryFact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    acknowledgements: Vec<IdempotencyAck>,
}

fn schema_is_legacy(version: &u32) -> bool {
    *version < SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IdempotencyAck {
    key: String,
    payload_digest: String,
    fact_id: String,
    revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: String,
    pub text: String,
    pub tags: Vec<String>,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    valid_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_criticality")]
    criticality: MemoryCriticality,
    #[serde(default, skip_serializing_if = "is_default_salience")]
    salience: MemorySalience,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<PersistedSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim_key: Option<String>,
}

impl Default for MemoryFact {
    fn default() -> Self {
        Self {
            id: String::new(),
            text: String::new(),
            tags: Vec::new(),
            updated_at: String::new(),
            revision: 0,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            criticality: MemoryCriticality::Normal,
            salience: MemorySalience::Medium,
            source: None,
            claim_key: None,
        }
    }
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn is_default_criticality(value: &MemoryCriticality) -> bool {
    matches!(value, MemoryCriticality::Normal)
}

fn is_default_salience(value: &MemorySalience) -> bool {
    matches!(value, MemorySalience::Medium)
}

impl MemoryFact {
    fn effective_revision(&self) -> u64 {
        self.revision.max(1)
    }

    fn is_critical(&self) -> bool {
        matches!(self.criticality, MemoryCriticality::Critical)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryCriticality {
    #[default]
    Normal,
    Critical,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemorySalience {
    Low,
    #[default]
    Medium,
    High,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemorySourceKind {
    #[default]
    Caller,
    Compaction,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedSource {
    kind: MemorySourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionedWriteRequest {
    idempotency_key: String,
    text: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    valid_from: Option<String>,
    #[serde(default)]
    valid_until: Option<String>,
    #[serde(default)]
    supersedes: Option<String>,
    #[serde(default)]
    criticality: RequestCriticality,
    #[serde(default)]
    salience: RequestSalience,
    #[serde(default)]
    source: RequestSource,
    #[serde(default)]
    claim_key: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestCriticality {
    #[default]
    Normal,
    Critical,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestSalience {
    Low,
    #[default]
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestSource {
    kind: RequestSourceKind,
    #[serde(default)]
    actor: Option<String>,
}

impl Default for RequestSource {
    fn default() -> Self {
        Self {
            kind: RequestSourceKind::Caller,
            actor: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestSourceKind {
    #[default]
    Caller,
    Compaction,
}

#[derive(Debug, Clone, Serialize)]
struct VersionedWriteAck {
    id: String,
    revision: u64,
    replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum MemoryError {
    #[error("malformed memory write")]
    Malformed,
    #[error("memory idempotency conflict")]
    IdempotencyConflict,
    #[error("memory hot store at capacity")]
    Capacity,
    #[error("memory reference crosses canonical scope")]
    CrossScope,
    #[error("memory supersession cycle")]
    Cycle,
    #[error("durable memory store error")]
    Durable,
}

impl MemoryError {
    fn code(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Capacity => "capacity",
            Self::CrossScope => "cross_scope",
            Self::Cycle => "cycle",
            Self::Durable => "durable",
        }
    }

    fn json(self) -> serde_json::Value {
        serde_json::json!({ "ok": false, "code": self.code() })
    }
}

fn classify_remember_error(error: &anyhow::Error) -> MemoryError {
    let message = format!("{error:#}");
    if message.contains("empty memory fact") || message.contains("malformed") {
        MemoryError::Malformed
    } else if message.contains("at capacity") {
        MemoryError::Capacity
    } else {
        MemoryError::Durable
    }
}

struct AuthoritativeRetrieval {
    at: DateTime<Utc>,
    current: Vec<MemoryFact>,
    conflicts: Vec<(String, Vec<MemoryFact>)>,
}

impl AuthoritativeRetrieval {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "at": format_timestamp(self.at),
            "current": self.current.iter().map(fact_view).collect::<Vec<_>>(),
            "conflicts": self.conflicts.iter().map(|(claim_key, facts)| {
                serde_json::json!({
                    "claim_key": claim_key,
                    "facts": facts.iter().map(fact_view).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

fn fact_view(fact: &MemoryFact) -> serde_json::Value {
    serde_json::json!({
        "id": fact.id,
        "text": fact.text,
        "tags": fact.tags,
        "updated_at": fact.updated_at,
        "revision": fact.effective_revision(),
        "valid_from": fact.valid_from,
        "valid_until": fact.valid_until,
        "supersedes": fact.supersedes,
        "criticality": match fact.criticality {
            MemoryCriticality::Critical => "critical",
            _ => "normal",
        },
        "salience": match fact.salience {
            MemorySalience::Low => "low",
            MemorySalience::High => "high",
            _ => "medium",
        },
        "source": fact.source.as_ref().map(|source| {
            serde_json::json!({
                "kind": match source.kind {
                    MemorySourceKind::Compaction => "compaction",
                    _ => "caller",
                },
                "actor": source.actor,
            })
        }),
        "claim_key": fact.claim_key,
    })
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
        schema_version: SCHEMA_VERSION,
        facts: Vec::new(),
        acknowledgements: Vec::new(),
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

fn format_timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

fn bounded_chars(value: &str, max_chars: usize) -> bool {
    value.chars().count() <= max_chars
}

fn canonical_payload_digest(request: &VersionedWriteRequest) -> String {
    let mut tags = request.tags.clone();
    tags.sort();
    let mut fields = BTreeMap::new();
    fields.insert(
        "claim_key",
        serde_json::to_value(&request.claim_key).unwrap(),
    );
    fields.insert(
        "criticality",
        serde_json::to_value(&request.criticality).unwrap(),
    );
    fields.insert("salience", serde_json::to_value(&request.salience).unwrap());
    fields.insert("source", serde_json::to_value(&request.source).unwrap());
    fields.insert(
        "supersedes",
        serde_json::to_value(&request.supersedes).unwrap(),
    );
    fields.insert("tags", serde_json::json!(tags));
    fields.insert("text", serde_json::json!(request.text));
    fields.insert(
        "valid_from",
        serde_json::to_value(&request.valid_from).unwrap(),
    );
    fields.insert(
        "valid_until",
        serde_json::to_value(&request.valid_until).unwrap(),
    );
    let mut map = serde_json::Map::new();
    for (key, value) in fields {
        map.insert(key.to_string(), value);
    }
    let digest = Sha256::digest(ValueObject(map).encode().as_bytes());
    format!("{digest:x}")
}

struct ValueObject(serde_json::Map<String, serde_json::Value>);

impl ValueObject {
    fn encode(&self) -> String {
        serde_json::Value::Object(self.0.clone()).to_string()
    }
}

fn fact_id_for(address: &MemoryAddress, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-memory-fact-v2\0");
    hasher.update(storage_key(address).as_bytes());
    hasher.update(b"\0");
    hasher.update(address.scope().label().as_bytes());
    hasher.update(b"\0");
    match address.scope() {
        MemoryScope::Project => {}
        MemoryScope::AgentPrivate { agent_id } => hasher.update(agent_id.as_bytes()),
        MemoryScope::Team { team_id } => hasher.update(team_id.as_bytes()),
    }
    hasher.update(b"\0");
    hasher.update(idempotency_key.as_bytes());
    format!("{FACT_ID_PREFIX}{:x}", hasher.finalize())
}

fn validate_versioned_request(
    request: &VersionedWriteRequest,
    now: DateTime<Utc>,
) -> Result<(DateTime<Utc>, Option<DateTime<Utc>>, Option<String>), MemoryError> {
    if request.idempotency_key.is_empty() || !bounded_chars(&request.idempotency_key, MAX_KEY_CHARS)
    {
        return Err(MemoryError::Malformed);
    }
    let text = request.text.trim();
    if text.is_empty() || text.chars().count() > MAX_FACT_CHARS {
        return Err(MemoryError::Malformed);
    }
    if request.tags.len() > MAX_TAGS {
        return Err(MemoryError::Malformed);
    }
    for tag in &request.tags {
        if tag.is_empty() || !bounded_chars(tag, MAX_TAG_CHARS) {
            return Err(MemoryError::Malformed);
        }
    }
    if let Some(actor) = &request.source.actor {
        if actor.is_empty() || !bounded_chars(actor, MAX_SOURCE_ACTOR_CHARS) {
            return Err(MemoryError::Malformed);
        }
    }
    let claim_key = request
        .claim_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    if let Some(claim_key) = &claim_key {
        if !bounded_chars(claim_key, MAX_KEY_CHARS) {
            return Err(MemoryError::Malformed);
        }
    }
    if let Some(supersedes) = &request.supersedes {
        if supersedes.is_empty() || !bounded_chars(supersedes, MAX_ID_CHARS) {
            return Err(MemoryError::Malformed);
        }
    }
    let valid_from = match &request.valid_from {
        Some(raw) => parse_timestamp(raw).ok_or(MemoryError::Malformed)?,
        None => now,
    };
    let valid_until = match &request.valid_until {
        Some(raw) => Some(parse_timestamp(raw).ok_or(MemoryError::Malformed)?),
        None => None,
    };
    if let Some(until) = valid_until {
        if valid_from >= until {
            return Err(MemoryError::Malformed);
        }
    }
    Ok((valid_from, valid_until, claim_key))
}

fn supersedes_cycle(facts: &[MemoryFact], new_id: &str, supersedes: &str) -> bool {
    if new_id == supersedes {
        return true;
    }
    let mut seen = HashSet::new();
    let mut cursor = Some(supersedes.to_string());
    while let Some(id) = cursor {
        if id == new_id {
            return true;
        }
        if !seen.insert(id.clone()) {
            return true;
        }
        cursor = facts
            .iter()
            .find(|fact| fact.id == id)
            .and_then(|fact| fact.supersedes.clone());
    }
    false
}

fn is_expired(fact: &MemoryFact, at: DateTime<Utc>) -> bool {
    fact.valid_until
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|until| at >= until)
}

fn is_active(fact: &MemoryFact, at: DateTime<Utc>) -> bool {
    let from_ok = fact
        .valid_from
        .as_deref()
        .and_then(parse_timestamp)
        .is_none_or(|from| from <= at);
    from_ok && !is_expired(fact, at)
}

fn superseded_ids(facts: &[MemoryFact]) -> HashSet<String> {
    facts
        .iter()
        .filter_map(|fact| fact.supersedes.clone())
        .collect()
}

fn is_protected(fact: &MemoryFact, at: DateTime<Utc>, superseded: &HashSet<String>) -> bool {
    fact.is_critical() && !is_expired(fact, at) && !superseded.contains(&fact.id)
}

fn salience_rank(salience: MemorySalience) -> u8 {
    match salience {
        MemorySalience::Low | MemorySalience::Unknown => 0,
        MemorySalience::Medium => 1,
        MemorySalience::High => 2,
    }
}

fn retrieval_score(fact: &MemoryFact) -> u32 {
    let salience = u32::from(salience_rank(fact.salience));
    let critical = if fact.is_critical() { 8 } else { 0 };
    salience + critical
}

fn sort_authoritative(facts: &mut [MemoryFact]) {
    facts.sort_by(|left, right| {
        retrieval_score(right)
            .cmp(&retrieval_score(left))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn encoded_len(memory: &ProjectMemory) -> Result<usize, MemoryError> {
    serde_json::to_vec_pretty(memory)
        .map(|raw| raw.len())
        .map_err(|_| MemoryError::Durable)
}

fn over_capacity(memory: &ProjectMemory) -> Result<bool, MemoryError> {
    if memory.facts.len() > MAX_FACTS {
        return Ok(true);
    }
    Ok(encoded_len(memory)? > MAX_HOT_STORE_BYTES)
}

fn pick_expired(facts: &[MemoryFact], at: DateTime<Utc>) -> Option<usize> {
    facts
        .iter()
        .enumerate()
        .filter(|(_, fact)| is_expired(fact, at))
        .min_by(|(_, left), (_, right)| {
            left.updated_at
                .cmp(&right.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|(index, _)| index)
}

fn pick_superseded(facts: &[MemoryFact]) -> Option<usize> {
    let superseded = superseded_ids(facts);
    facts
        .iter()
        .enumerate()
        .filter(|(_, fact)| superseded.contains(&fact.id))
        .min_by(|(_, left), (_, right)| {
            left.updated_at
                .cmp(&right.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|(index, _)| index)
}

fn pick_noncritical(facts: &[MemoryFact], at: DateTime<Utc>) -> Option<usize> {
    let superseded = superseded_ids(facts);
    facts
        .iter()
        .enumerate()
        .filter(|(_, fact)| !is_protected(fact, at, &superseded))
        .min_by(|(_, left), (_, right)| {
            salience_rank(left.salience)
                .cmp(&salience_rank(right.salience))
                .then_with(|| left.updated_at.cmp(&right.updated_at))
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|(index, _)| index)
}

fn drop_acknowledgements(memory: &mut ProjectMemory) {
    let ids: HashSet<&str> = memory.facts.iter().map(|fact| fact.id.as_str()).collect();
    memory
        .acknowledgements
        .retain(|ack| ids.contains(ack.fact_id.as_str()));
}

fn compact(memory: &mut ProjectMemory, at: DateTime<Utc>) -> Result<(), MemoryError> {
    while over_capacity(memory)? {
        let index = pick_expired(&memory.facts, at)
            .or_else(|| pick_superseded(&memory.facts))
            .or_else(|| pick_noncritical(&memory.facts, at));
        let Some(index) = index else {
            return Err(MemoryError::Capacity);
        };
        memory.facts.remove(index);
        drop_acknowledgements(memory);
    }
    memory
        .acknowledgements
        .sort_by(|left, right| left.key.cmp(&right.key));
    Ok(())
}

fn persist_copy(
    address: &MemoryAddress,
    path: &Path,
    mut memory: ProjectMemory,
    incoming: MemoryFact,
    acknowledgement: Option<IdempotencyAck>,
    now: DateTime<Utc>,
) -> Result<(), MemoryError> {
    let incoming_id = incoming.id.clone();
    memory.facts.push(incoming);
    if let Some(acknowledgement) = acknowledgement {
        memory
            .acknowledgements
            .retain(|ack| ack.key != acknowledgement.key);
        memory.acknowledgements.push(acknowledgement);
    }
    drop_acknowledgements(&mut memory);
    compact(&mut memory, now)?;
    if !memory.facts.iter().any(|fact| fact.id == incoming_id) {
        return Err(MemoryError::Capacity);
    }
    if over_capacity(&memory)? {
        return Err(MemoryError::Capacity);
    }
    memory.schema_version = SCHEMA_VERSION;
    memory.project_key = storage_key(address);
    memory.cwd = address.source_workspace().display().to_string();
    save_to_path(address, path, &memory).map_err(|_| MemoryError::Durable)
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
    let memory = load_from_path(address, &path)?.unwrap_or_else(|| empty_memory(address));
    let now = address.clock.now();
    if let Some(existing) = memory.facts.iter().find(|fact| {
        fact.text == text && is_active(fact, now) && {
            let superseded = superseded_ids(&memory.facts);
            !superseded.contains(&fact.id)
        }
    }) {
        return Ok(existing.id.clone());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let stamped = format_timestamp(now);
    let incoming = MemoryFact {
        id: id.clone(),
        text,
        tags: tags.to_vec(),
        updated_at: stamped.clone(),
        revision: 1,
        valid_from: Some(stamped),
        valid_until: None,
        supersedes: None,
        criticality: MemoryCriticality::Normal,
        salience: MemorySalience::Medium,
        source: Some(PersistedSource {
            kind: MemorySourceKind::Caller,
            actor: address.actor_agent_id.clone(),
        }),
        claim_key: None,
    };
    persist_copy(address, &path, memory, incoming, None, now).map_err(|error| anyhow!(error))?;
    Ok(id)
}

fn remember_versioned(
    address: &MemoryAddress,
    request: VersionedWriteRequest,
) -> Result<VersionedWriteAck, MemoryError> {
    let now = address.clock.now();
    let (valid_from, valid_until, claim_key) = validate_versioned_request(&request, now)?;
    let digest = canonical_payload_digest(&request);
    let path = path_for(address).map_err(|_| MemoryError::Durable)?;
    let lock = address_lock(&path);
    let _guard = lock.lock();
    let original = fs::read(&path).ok();
    let memory = match load_from_path(address, &path) {
        Ok(Some(memory)) => memory,
        Ok(None) => empty_memory(address),
        Err(_) => return Err(MemoryError::Durable),
    };
    if let Some(existing) = memory
        .acknowledgements
        .iter()
        .find(|ack| ack.key == request.idempotency_key)
    {
        if existing.payload_digest != digest {
            return Err(MemoryError::IdempotencyConflict);
        }
        return Ok(VersionedWriteAck {
            id: existing.fact_id.clone(),
            revision: existing.revision,
            replayed: true,
        });
    }
    let id = fact_id_for(address, &request.idempotency_key);
    if !bounded_chars(&id, MAX_ID_CHARS) {
        return Err(MemoryError::Malformed);
    }
    if let Some(supersedes) = &request.supersedes {
        if supersedes == &id || supersedes_cycle(&memory.facts, &id, supersedes) {
            return Err(MemoryError::Cycle);
        }
        if !memory.facts.iter().any(|fact| fact.id == *supersedes) {
            return Err(MemoryError::CrossScope);
        }
    }
    let revision = request
        .supersedes
        .as_ref()
        .and_then(|target| {
            memory
                .facts
                .iter()
                .find(|fact| fact.id == *target)
                .map(|fact| fact.effective_revision().saturating_add(1))
        })
        .unwrap_or(1)
        .max(1);
    let incoming = MemoryFact {
        id: id.clone(),
        text: request.text.trim().to_string(),
        tags: request.tags.clone(),
        updated_at: format_timestamp(now),
        revision,
        valid_from: Some(format_timestamp(valid_from)),
        valid_until: valid_until.map(format_timestamp),
        supersedes: request.supersedes.clone(),
        criticality: match request.criticality {
            RequestCriticality::Critical => MemoryCriticality::Critical,
            RequestCriticality::Normal => MemoryCriticality::Normal,
        },
        salience: match request.salience {
            RequestSalience::Low => MemorySalience::Low,
            RequestSalience::Medium => MemorySalience::Medium,
            RequestSalience::High => MemorySalience::High,
        },
        source: Some(PersistedSource {
            kind: match request.source.kind {
                RequestSourceKind::Caller => MemorySourceKind::Caller,
                RequestSourceKind::Compaction => MemorySourceKind::Compaction,
            },
            actor: request.source.actor.clone(),
        }),
        claim_key,
    };
    let acknowledgement = IdempotencyAck {
        key: request.idempotency_key.clone(),
        payload_digest: digest,
        fact_id: id.clone(),
        revision,
    };
    match persist_copy(address, &path, memory, incoming, Some(acknowledgement), now) {
        Ok(()) => Ok(VersionedWriteAck {
            id,
            revision,
            replayed: false,
        }),
        Err(error) => {
            match original {
                Some(raw) => {
                    if fs::read(&path).ok().as_deref() != Some(raw.as_slice()) {
                        let _ = fs::write(&path, raw);
                    }
                }
                None => {
                    if path.exists() {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
            Err(error)
        }
    }
}

pub(crate) fn list_facts(address: &MemoryAddress) -> anyhow::Result<Vec<MemoryFact>> {
    Ok(load(address)?.facts)
}

fn retrieve_at(
    address: &MemoryAddress,
    at: DateTime<Utc>,
) -> Result<AuthoritativeRetrieval, MemoryError> {
    let memory = load(address).map_err(|_| MemoryError::Durable)?;
    let superseded = superseded_ids(&memory.facts);
    let active: Vec<MemoryFact> = memory
        .facts
        .into_iter()
        .filter(|fact| is_active(fact, at) && !superseded.contains(&fact.id))
        .collect();
    let mut unkeyed = Vec::new();
    let mut by_claim: BTreeMap<String, Vec<MemoryFact>> = BTreeMap::new();
    for fact in active {
        match fact
            .claim_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            Some(claim_key) => by_claim
                .entry(claim_key.to_string())
                .or_default()
                .push(fact),
            None => unkeyed.push(fact),
        }
    }
    let mut current = unkeyed;
    let mut conflicts = Vec::new();
    for (claim_key, mut facts) in by_claim {
        let distinct_text: HashSet<&str> = facts.iter().map(|fact| fact.text.as_str()).collect();
        if facts.len() > 1 && distinct_text.len() > 1 {
            sort_authoritative(&mut facts);
            conflicts.push((claim_key, facts));
        } else {
            current.extend(facts);
        }
    }
    sort_authoritative(&mut current);
    Ok(AuthoritativeRetrieval {
        at,
        current,
        conflicts,
    })
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
/// match the legacy project-memory implementation, excluding facts that are
/// expired or superseded at the injected clock.
pub(crate) fn inject_context(address: &MemoryAddress) -> anyhow::Result<String> {
    let memory = load(address)?;
    if memory.facts.is_empty() {
        return Ok(String::new());
    }
    let now = address.clock.now();
    let superseded = superseded_ids(&memory.facts);
    let mut current: Vec<&MemoryFact> = memory
        .facts
        .iter()
        .filter(|fact| is_active(fact, now) && !superseded.contains(&fact.id))
        .collect();
    current.sort_by(|left, right| {
        retrieval_score(right)
            .cmp(&retrieval_score(left))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut out = String::from(
        "Project memory (facts from prior sessions on this project; honor unless user overrides):\n",
    );
    let mut used = out.len();
    for fact in current {
        let line = format!("- {}\n", fact.text);
        if used + line.len() > MAX_INJECT_CHARS {
            break;
        }
        out.push_str(&line);
        used += line.len();
    }
    if used == "Project memory (facts from prior sessions on this project; honor unless user overrides):\n".len()
    {
        return Ok(String::new());
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

    fn epoch() -> DateTime<Utc> {
        parse_timestamp("2000-01-01T00:00:00.000Z").unwrap()
    }

    fn access_at(source: &Path, actor: Option<String>, now: DateTime<Utc>) -> MemoryAccess {
        MemoryAccess::new(source, actor).with_clock(Arc::new(FakeClock::new(now)))
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
            schema_version: 0,
            facts: vec![MemoryFact {
                id: "foreign".into(),
                text: "must not leak".into(),
                tags: vec![],
                updated_at: "now".into(),
                ..Default::default()
            }],
            acknowledgements: Vec::new(),
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

    #[test]
    fn legacy_v1_facts_deserialize_and_remain_current() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let access = access_at(source.path(), None, epoch());
        let address = access.project();
        let path = path_for(&address).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let raw = serde_json::json!({
            "project_key": legacy_project_key(source.path()),
            "cwd": canonical_workspace(source.path()).display().to_string(),
            "facts": [{
                "id": "legacy-1",
                "text": "Always use tabs in this repo",
                "tags": ["style"],
                "updated_at": "2000-01-01T00:00:00Z"
            }]
        });
        fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        let facts = list_facts(&address).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].revision, 0);
        let retrieved = retrieve_at(&address, epoch()).unwrap();
        assert_eq!(retrieved.current.len(), 1);
        assert_eq!(retrieved.current[0].effective_revision(), 1);
        assert!(retrieved.conflicts.is_empty());
    }

    #[test]
    fn fake_clock_stamps_versioned_writes_without_sleeping() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let clock = Arc::new(FakeClock::new(epoch()));
        let address = MemoryAccess::new(source.path(), None)
            .with_clock(clock.clone())
            .project();
        let ack = remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "clock-stamp".into(),
                text: "stamped at epoch".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: None,
            },
        )
        .unwrap();
        assert!(!ack.replayed);
        let facts = list_facts(&address).unwrap();
        assert_eq!(facts[0].updated_at, format_timestamp(epoch()));
        clock.advance(Duration::days(365));
        remember(&address, "one logical year later", &[]).unwrap();
        assert_eq!(
            list_facts(&address).unwrap()[1].updated_at,
            format_timestamp(epoch() + Duration::days(365))
        );
        clock.set(epoch());
        let rolled_back = retrieve_at(&address, clock.now()).unwrap();
        assert_eq!(rolled_back.current.len(), 1);
        assert_eq!(rolled_back.current[0].text, "stamped at epoch");
    }

    #[test]
    fn idempotent_replay_returns_the_same_revision_and_conflict_rejects_payload_change() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let address = access_at(source.path(), None, epoch()).project();
        let request = VersionedWriteRequest {
            idempotency_key: "pref-indent".into(),
            text: "Use tabs.".into(),
            tags: vec!["style".into()],
            valid_from: None,
            valid_until: None,
            supersedes: None,
            criticality: RequestCriticality::Normal,
            salience: RequestSalience::High,
            source: RequestSource::default(),
            claim_key: Some("indent-style".into()),
        };
        let first = remember_versioned(&address, request.clone()).unwrap();
        let replay = remember_versioned(&address, request.clone()).unwrap();
        assert!(replay.replayed);
        assert_eq!(first.id, replay.id);
        assert_eq!(first.revision, replay.revision);
        assert_eq!(list_facts(&address).unwrap().len(), 1);
        let mut changed = request;
        changed.text = "Use spaces.".into();
        assert_eq!(
            remember_versioned(&address, changed).unwrap_err(),
            MemoryError::IdempotencyConflict
        );
        assert_eq!(list_facts(&address).unwrap().len(), 1);
    }

    #[test]
    fn malformed_versioned_write_never_mutates_the_canonical_file() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let address = access_at(source.path(), None, epoch()).project();
        remember(&address, "seed", &[]).unwrap();
        let path = path_for(&address).unwrap();
        let before = fs::read(&path).unwrap();
        let oversized: String = "x".repeat(MAX_FACT_CHARS + 1);
        let err = remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "too-long".into(),
                text: oversized,
                ..VersionedWriteRequest {
                    idempotency_key: "too-long".into(),
                    text: "placeholder".into(),
                    tags: vec![],
                    valid_from: None,
                    valid_until: None,
                    supersedes: None,
                    criticality: RequestCriticality::Normal,
                    salience: RequestSalience::Medium,
                    source: RequestSource::default(),
                    claim_key: None,
                }
            },
        )
        .unwrap_err();
        assert_eq!(err, MemoryError::Malformed);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn eighty_first_noncritical_write_compacts_without_dropping_current_critical() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let clock = Arc::new(FakeClock::new(epoch()));
        let address = MemoryAccess::new(source.path(), None)
            .with_clock(clock.clone())
            .project();
        remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "critical-license".into(),
                text: "License is Apache-2.0.".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Critical,
                salience: RequestSalience::High,
                source: RequestSource::default(),
                claim_key: Some("workspace-license".into()),
            },
        )
        .unwrap();
        for index in 0..79 {
            clock.advance(Duration::seconds(1));
            remember_versioned(
                &address,
                VersionedWriteRequest {
                    idempotency_key: format!("filler-{index}"),
                    text: format!("filler {index}"),
                    tags: vec![],
                    valid_from: None,
                    valid_until: None,
                    supersedes: None,
                    criticality: RequestCriticality::Normal,
                    salience: RequestSalience::Low,
                    source: RequestSource::default(),
                    claim_key: None,
                },
            )
            .unwrap();
        }
        assert_eq!(list_facts(&address).unwrap().len(), 80);
        clock.advance(Duration::seconds(1));
        remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "filler-80".into(),
                text: "filler 80".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Low,
                source: RequestSource::default(),
                claim_key: None,
            },
        )
        .unwrap();
        let facts = list_facts(&address).unwrap();
        assert_eq!(facts.len(), 80);
        assert!(facts
            .iter()
            .any(|fact| fact.claim_key.as_deref() == Some("workspace-license")));
        let retrieved = retrieve_at(&address, clock.now()).unwrap();
        assert!(retrieved
            .current
            .iter()
            .any(|fact| fact.claim_key.as_deref() == Some("workspace-license")));
    }

    #[test]
    fn current_critical_capacity_rejects_without_mutation() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let clock = Arc::new(FakeClock::new(epoch()));
        let address = MemoryAccess::new(source.path(), None)
            .with_clock(clock.clone())
            .project();
        for index in 0..80 {
            clock.advance(Duration::seconds(1));
            remember_versioned(
                &address,
                VersionedWriteRequest {
                    idempotency_key: format!("crit-{index}"),
                    text: format!("critical invariant {index}"),
                    tags: vec![],
                    valid_from: None,
                    valid_until: None,
                    supersedes: None,
                    criticality: RequestCriticality::Critical,
                    salience: RequestSalience::High,
                    source: RequestSource::default(),
                    claim_key: Some(format!("inv-{index}")),
                },
            )
            .unwrap();
        }
        let path = path_for(&address).unwrap();
        let before = fs::read(&path).unwrap();
        clock.advance(Duration::seconds(1));
        let err = remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "crit-80".into(),
                text: "one more critical".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Critical,
                salience: RequestSalience::High,
                source: RequestSource::default(),
                claim_key: Some("inv-80".into()),
            },
        )
        .unwrap_err();
        assert_eq!(err, MemoryError::Capacity);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn retrieve_excludes_expired_and_superseded_and_surfaces_conflicts() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let clock = Arc::new(FakeClock::new(epoch()));
        let address = MemoryAccess::new(source.path(), None)
            .with_clock(clock.clone())
            .project();
        let first = remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "indent-v1".into(),
                text: "Use tabs.".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: Some("indent-style".into()),
            },
        )
        .unwrap();
        clock.advance(Duration::days(365));
        remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "indent-v2".into(),
                text: "Use spaces.".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: Some(first.id.clone()),
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: Some("indent-style".into()),
            },
        )
        .unwrap();
        remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "window".into(),
                text: "Deploy window is open.".into(),
                tags: vec![],
                valid_from: None,
                valid_until: Some(format_timestamp(clock.now() + Duration::days(30))),
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Low,
                source: RequestSource::default(),
                claim_key: Some("deploy-window".into()),
            },
        )
        .unwrap();
        remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "channel-a".into(),
                text: "Release channel is stable.".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: Some("release-channel".into()),
            },
        )
        .unwrap();
        remember_versioned(
            &address,
            VersionedWriteRequest {
                idempotency_key: "channel-b".into(),
                text: "Release channel is beta.".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: Some("release-channel".into()),
            },
        )
        .unwrap();
        let at_year_two = epoch() + Duration::days(365 * 2);
        let retrieved = retrieve_at(&address, at_year_two).unwrap();
        assert!(!retrieved
            .current
            .iter()
            .any(|fact| fact.id == first.id || fact.claim_key.as_deref() == Some("deploy-window")));
        assert_eq!(
            retrieved
                .current
                .iter()
                .find(|fact| fact.claim_key.as_deref() == Some("indent-style"))
                .map(|fact| fact.text.as_str()),
            Some("Use spaces.")
        );
        assert_eq!(retrieved.conflicts.len(), 1);
        assert_eq!(retrieved.conflicts[0].0, "release-channel");
        assert_eq!(retrieved.conflicts[0].1.len(), 2);
    }

    #[test]
    fn cyclic_and_cross_scope_supersession_do_not_mutate() {
        let _home = IsolatedHome::install();
        let source = tempfile::tempdir().unwrap();
        let project = access_at(source.path(), Some("agent-one".into()), epoch()).project();
        let private = access_at(source.path(), Some("agent-one".into()), epoch())
            .resolve(MemoryScope::AgentPrivate {
                agent_id: "agent-one".into(),
            })
            .unwrap();
        let first = remember_versioned(
            &project,
            VersionedWriteRequest {
                idempotency_key: "a".into(),
                text: "alpha".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: None,
            },
        )
        .unwrap();
        let second = remember_versioned(
            &project,
            VersionedWriteRequest {
                idempotency_key: "b".into(),
                text: "beta".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: Some(first.id.clone()),
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: None,
            },
        )
        .unwrap();
        let path = path_for(&project).unwrap();
        let before = fs::read(&path).unwrap();
        let _ = second;
        let self_cycle = remember_versioned(
            &project,
            VersionedWriteRequest {
                idempotency_key: "self".into(),
                text: "self".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: Some(fact_id_for(&project, "self")),
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: None,
            },
        )
        .unwrap_err();
        assert_eq!(self_cycle, MemoryError::Cycle);
        let private_fact = remember_versioned(
            &private,
            VersionedWriteRequest {
                idempotency_key: "private".into(),
                text: "private only".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: None,
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: None,
            },
        )
        .unwrap();
        let cross = remember_versioned(
            &project,
            VersionedWriteRequest {
                idempotency_key: "cross".into(),
                text: "should not land".into(),
                tags: vec![],
                valid_from: None,
                valid_until: None,
                supersedes: Some(private_fact.id),
                criticality: RequestCriticality::Normal,
                salience: RequestSalience::Medium,
                source: RequestSource::default(),
                claim_key: None,
            },
        )
        .unwrap_err();
        assert_eq!(cross, MemoryError::CrossScope);
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}
