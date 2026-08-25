//! Serializable swarm specification DTOs.
//!
//! These are the contract a coordinator writes once and a durable store keeps.
//! They describe *what may be dispatched*, never *what is authorized*: every
//! authority in this module is a requirement or a reference to an authority
//! granted elsewhere. Nothing here can widen a child's permissions.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use xai_tool_types::{SubagentCapabilityMode, SubagentIsolationMode};

use crate::error::{SwarmError, SwarmResult};
use crate::ids::{
    CredentialRef, DispatchId, ExternalRefId, LeaseId, ModelId, ProviderId, SwarmId, TaskId,
    WorkerId,
};
use crate::policy::{
    AdmissionPolicy, BudgetPolicy, FailurePolicy, MAX_DEPENDENCIES, MAX_TASKS, MAX_WORKERS,
    ReviewGate,
};

/// Version of the durable swarm record shape.
pub const SWARM_SCHEMA_VERSION: u32 = 2;

/// Maximum bytes in a swarm objective.
pub const MAX_OBJECTIVE_BYTES: usize = 8 * 1024;
/// Maximum bytes in a task title.
pub const MAX_TITLE_BYTES: usize = 256;
/// Maximum bytes in task instructions.
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;
/// Maximum entries in a provider capability catalog.
pub const MAX_CATALOG_ENTRIES: usize = 64;

/// Reject empty, oversized, or control-character-bearing operator text.
pub(crate) fn validate_text(value: &str, field: &str, max_bytes: usize) -> SwarmResult<()> {
    if value.trim().is_empty() {
        return Err(SwarmError::invalid(format!("{field} must not be blank")));
    }
    if value.len() > max_bytes {
        return Err(SwarmError::invalid(format!(
            "{field} exceeds {max_bytes} bytes"
        )));
    }
    if value
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(SwarmError::invalid(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

/// What a worker is allowed to do.
///
/// This set is deliberately closed. There is no browser-automation variant and
/// no raw-host variant, so no swarm specification — however it was authored or
/// deserialized — can express either authority. Physical screen, keyboard, and
/// pointer access is reachable only through [`WorkerCapability::ComputerUseLeased`],
/// which is inert without a separately issued operator lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCapability {
    /// Read files in the assigned working folder.
    ReadWorkspace,
    /// Modify files in the assigned working folder. Requires worktree isolation.
    WriteWorkspace,
    /// Run commands inside the isolated worktree, under the host's existing
    /// permission and safety controls. Requires worktree isolation.
    ExecuteInWorktree,
    /// Produce a review verdict on another task's result.
    Review,
    /// Combine reviewed results into one output.
    Synthesize,
    /// Act on the operator's computer *only* under a lease issued by the local
    /// operator. The control plane records the requirement and the reference;
    /// it never issues, extends, or revalidates the lease itself.
    ComputerUseLeased,
}

/// The part a worker plays in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    Implementer,
    Explorer,
    Reviewer,
    Synthesizer,
}

/// Working-folder isolation a worker requires.
///
/// This mirrors the repository's subagent isolation rule: read-only children
/// may stay in the parent folder, and any mutating child gets its own worktree.
/// Worktree separation prevents routine edit collisions; it is not an
/// operating-system sandbox, and this type never claims otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationRequirement {
    /// Read-only child that may share the parent working folder.
    SharedReadOnly,
    /// Dedicated detached worktree; the child's edits never land in the parent
    /// folder until they are explicitly promoted.
    Worktree,
}

impl IsolationRequirement {
    /// Project onto the existing subagent isolation wire enum so a dispatcher
    /// can hand this straight to the current task tool.
    pub fn as_subagent_isolation(self) -> SubagentIsolationMode {
        match self {
            Self::SharedReadOnly => SubagentIsolationMode::None,
            Self::Worktree => SubagentIsolationMode::Worktree,
        }
    }
}

/// Who issued a Computer Use lease. Only a local operator can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseIssuer {
    /// Explicit authorization made through the local GrokPtah operator surface.
    #[serde(alias = "local_operator")]
    LocalUser,
}

/// Action class an external Computer Use grant authorizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseActionClass {
    Semantic,
    TextEntry,
    KeyChord,
    PointerFallback,
}

/// External Computer Use authority required by one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerUseRequirement {
    /// Opaque identifier of the externally owned Computer Run.
    pub run_id: ExternalRefId,
    /// Opaque canonical identifier of the exact Computer Use target.
    pub target_ref: ExternalRefId,
    /// Opaque identity of the owner/session to which the grant is bound.
    pub owner_ref: ExternalRefId,
    pub action_class: ComputerUseActionClass,
}

impl ComputerUseRequirement {
    pub fn validate(&self) -> SwarmResult<()> {
        self.run_id.validate()?;
        self.target_ref.validate()?;
        self.owner_ref.validate()
    }
}

/// A reference to a Computer Use lease issued outside this crate.
///
/// The control plane treats a lease as evidence, not as a grant it can mint.
/// A task that requires Computer Use is undispatchable until the caller
/// attaches a lease reference that is unexpired, unrevoked, and has uses left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerUseLeaseRef {
    /// Existing external grant identity. This crate does not issue it.
    pub lease_id: LeaseId,
    pub issued_by: LeaseIssuer,
    /// Exact swarm/task/dispatch binding minted by the external authority.
    pub swarm_id: SwarmId,
    pub task_id: TaskId,
    pub dispatch_id: DispatchId,
    /// Bindings corresponding to the existing ActionGrant/ComputerRun
    /// contract. These are opaque so this crate remains independent of the
    /// Computer Use runtime.
    pub run_id: ExternalRefId,
    pub target_ref: ExternalRefId,
    pub owner_ref: ExternalRefId,
    pub action_classes: BTreeSet<ComputerUseActionClass>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses_remaining: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ComputerUseLeaseRef {
    pub fn validate(&self) -> SwarmResult<()> {
        self.lease_id.validate()?;
        self.swarm_id.validate()?;
        self.task_id.validate()?;
        self.dispatch_id.validate()?;
        self.run_id.validate()?;
        self.target_ref.validate()?;
        self.owner_ref.validate()?;
        if self.action_classes.is_empty() {
            return Err(SwarmError::invalid(
                "computer-use lease must authorize an action class",
            ));
        }
        if self.expires_at <= self.issued_at {
            return Err(SwarmError::invalid(
                "computer-use lease must have a positive lifetime",
            ));
        }
        if self.uses_remaining == Some(0) {
            return Err(SwarmError::invalid(
                "computer-use lease has no remaining uses",
            ));
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        requirement: &ComputerUseRequirement,
        swarm_id: &SwarmId,
        task_id: &TaskId,
        dispatch_id: &DispatchId,
        now: DateTime<Utc>,
    ) -> SwarmResult<()> {
        self.validate()?;
        requirement.validate()?;
        if self.swarm_id != *swarm_id
            || self.task_id != *task_id
            || self.dispatch_id != *dispatch_id
        {
            return Err(SwarmError::capability(
                "Computer Use lease is not bound to this swarm, task, and dispatch",
            ));
        }
        if self.run_id != requirement.run_id
            || self.target_ref != requirement.target_ref
            || self.owner_ref != requirement.owner_ref
            || !self.action_classes.contains(&requirement.action_class)
        {
            return Err(SwarmError::capability(
                "Computer Use lease does not match the task's external authority binding",
            ));
        }
        if !self.is_usable_at(now) {
            return Err(SwarmError::capability(
                "the supplied Computer Use lease is expired, revoked, spent, or not yet issued",
            ));
        }
        Ok(())
    }

    /// True only when the lease is structurally valid, unrevoked, and live at
    /// `now`. An absent or malformed lease is never usable.
    pub fn is_usable_at(&self, now: DateTime<Utc>) -> bool {
        self.validate().is_ok()
            && self.revoked_at.is_none()
            && now >= self.issued_at
            && now < self.expires_at
    }
}

/// One measured provider/model capability record.
///
/// Presence in the catalog is the *only* thing that makes a provider, model,
/// role, or capability usable. A name is never treated as proof of capability,
/// matching how the repository qualifies models before enabling them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalogEntry {
    pub provider: ProviderId,
    pub model: ModelId,
    /// Roles this exact provider/model pair has been measured to fill.
    pub roles: BTreeSet<WorkerRole>,
    /// Capabilities this exact provider/model pair has been measured to hold.
    pub capabilities: BTreeSet<WorkerCapability>,
    /// Capability modes this pair may be dispatched with.
    pub capability_modes: Vec<SubagentCapabilityMode>,
}

impl ProviderCatalogEntry {
    pub fn validate(&self) -> SwarmResult<()> {
        self.provider.validate()?;
        self.model.validate()?;
        if self.roles.is_empty() {
            return Err(SwarmError::invalid("catalog entry must measure a role"));
        }
        if self.capabilities.is_empty() {
            return Err(SwarmError::invalid(
                "catalog entry must measure a capability",
            ));
        }
        if self.capability_modes.is_empty() {
            return Err(SwarmError::invalid(
                "catalog entry must measure a capability mode",
            ));
        }
        for (index, mode) in self.capability_modes.iter().enumerate() {
            if self.capability_modes[..index].contains(mode) {
                return Err(SwarmError::invalid(
                    "catalog entry must not repeat a capability mode",
                ));
            }
        }
        Ok(())
    }

    fn allows_mode(&self, mode: SubagentCapabilityMode) -> bool {
        self.capability_modes.contains(&mode)
    }
}

/// The measured provider surface a swarm may draw on.
///
/// An empty catalog admits nothing. That is the intended default: capability
/// is granted by measurement, never by omission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalog {
    #[serde(default)]
    pub entries: Vec<ProviderCatalogEntry>,
}

impl ProviderCatalog {
    pub fn new(entries: Vec<ProviderCatalogEntry>) -> Self {
        Self { entries }
    }

    pub fn validate(&self) -> SwarmResult<()> {
        if self.entries.len() > MAX_CATALOG_ENTRIES {
            return Err(SwarmError::bound(format!(
                "catalog may hold at most {MAX_CATALOG_ENTRIES} entries"
            )));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.entries {
            entry.validate()?;
            if !seen.insert((entry.provider.clone(), entry.model.clone())) {
                return Err(SwarmError::invalid(
                    "catalog must not repeat a provider/model pair",
                ));
            }
        }
        Ok(())
    }

    /// Look up the measured record for an exact provider/model pair.
    pub fn entry(&self, provider: &ProviderId, model: &ModelId) -> Option<&ProviderCatalogEntry> {
        self.entries
            .iter()
            .find(|entry| &entry.provider == provider && &entry.model == model)
    }
}

/// One worker a swarm may dispatch tasks to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerSpec {
    pub worker_id: WorkerId,
    pub provider: ProviderId,
    pub model: ModelId,
    pub role: WorkerRole,
    pub capability_mode: SubagentCapabilityMode,
    pub capabilities: BTreeSet<WorkerCapability>,
    pub isolation: IsolationRequirement,
    /// Name of a credential held in the OS keychain or host configuration.
    ///
    /// This is a reference, never a secret value, and it is omitted from every
    /// public projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<CredentialRef>,
}

impl WorkerSpec {
    /// Structural checks that do not consult the catalog.
    pub fn validate_shape(&self) -> SwarmResult<()> {
        self.worker_id.validate()?;
        self.provider.validate()?;
        self.model.validate()?;
        if let Some(credential) = &self.credential_ref {
            credential.validate()?;
        }
        if self.capabilities.is_empty() {
            return Err(SwarmError::invalid(format!(
                "worker '{}' must declare a capability",
                self.worker_id
            )));
        }

        let mutates = self
            .capabilities
            .contains(&WorkerCapability::WriteWorkspace)
            || self
                .capabilities
                .contains(&WorkerCapability::ExecuteInWorktree);
        let read_only = matches!(self.capability_mode, SubagentCapabilityMode::ReadOnly);

        if mutates && self.isolation != IsolationRequirement::Worktree {
            return Err(SwarmError::invalid(format!(
                "worker '{}' mutates the workspace and must require worktree isolation",
                self.worker_id
            )));
        }
        if mutates && read_only {
            return Err(SwarmError::invalid(format!(
                "worker '{}' declares mutating capabilities under a read-only capability mode",
                self.worker_id
            )));
        }
        if !read_only && self.isolation != IsolationRequirement::Worktree {
            return Err(SwarmError::invalid(format!(
                "worker '{}' is not read-only and must require worktree isolation",
                self.worker_id
            )));
        }
        match self.capability_mode {
            SubagentCapabilityMode::All => {
                return Err(SwarmError::capability(
                    "the unrestricted capability mode is not admissible in the closed swarm contract",
                ));
            }
            SubagentCapabilityMode::ReadWrite
                if !self
                    .capabilities
                    .contains(&WorkerCapability::WriteWorkspace)
                    && !self
                        .capabilities
                        .contains(&WorkerCapability::ExecuteInWorktree) =>
            {
                return Err(SwarmError::capability(format!(
                    "worker '{}' requests read-write mode without a declared mutating capability",
                    self.worker_id
                )));
            }
            SubagentCapabilityMode::Execute
                if !self
                    .capabilities
                    .contains(&WorkerCapability::ExecuteInWorktree) =>
            {
                return Err(SwarmError::capability(format!(
                    "worker '{}' requests execute mode without ExecuteInWorktree",
                    self.worker_id
                )));
            }
            _ => {}
        }
        if self
            .capabilities
            .contains(&WorkerCapability::ComputerUseLeased)
            && self.isolation != IsolationRequirement::Worktree
        {
            return Err(SwarmError::invalid(format!(
                "worker '{}' has Computer Use authority and must require worktree isolation",
                self.worker_id
            )));
        }
        Ok(())
    }

    /// Fail-closed check against the measured catalog.
    pub fn validate_against(&self, catalog: &ProviderCatalog) -> SwarmResult<()> {
        self.validate_shape()?;
        let Some(entry) = catalog.entry(&self.provider, &self.model) else {
            return Err(SwarmError::capability(format!(
                "worker '{}' names provider/model '{}/{}', which the catalog does not measure",
                self.worker_id, self.provider, self.model
            )));
        };
        if !entry.roles.contains(&self.role) {
            return Err(SwarmError::capability(format!(
                "provider/model '{}/{}' is not measured for the requested role",
                self.provider, self.model
            )));
        }
        if !entry.allows_mode(self.capability_mode) {
            return Err(SwarmError::capability(format!(
                "provider/model '{}/{}' is not measured for capability mode '{}'",
                self.provider,
                self.model,
                self.capability_mode.as_str()
            )));
        }
        for capability in &self.capabilities {
            if !entry.capabilities.contains(capability) {
                return Err(SwarmError::capability(format!(
                    "provider/model '{}/{}' is not measured for a requested capability",
                    self.provider, self.model
                )));
            }
        }
        Ok(())
    }

    /// True when this worker may be assigned Computer Use work.
    pub fn allows_computer_use(&self) -> bool {
        self.capabilities
            .contains(&WorkerCapability::ComputerUseLeased)
    }
}

/// What a graph node is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Ordinary implementation or exploration work.
    Work,
    /// Produces an approve/reject verdict on upstream work.
    Review,
    /// Combines reviewed results, gated by a quorum of reviewers.
    Synthesis,
}

impl TaskKind {
    fn accepts_role(self, role: WorkerRole) -> bool {
        match self {
            Self::Work => matches!(role, WorkerRole::Implementer | WorkerRole::Explorer),
            Self::Review => role == WorkerRole::Reviewer,
            Self::Synthesis => role == WorkerRole::Synthesizer,
        }
    }
}

/// One node in the task graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskSpec {
    pub task_id: TaskId,
    pub kind: TaskKind,
    pub title: String,
    pub instructions: String,
    pub worker_id: WorkerId,
    #[serde(default)]
    pub dependencies: Vec<TaskId>,
    /// Higher runs first when several tasks are ready at once. Ties break on
    /// task ID, so dispatch order is fully deterministic.
    #[serde(default)]
    pub priority: i32,
    /// When true the task cannot be dispatched without a usable Computer Use
    /// lease reference supplied by the caller at dispatch time.
    #[serde(default)]
    pub requires_computer_use: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<ComputerUseRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_gate: Option<ReviewGate>,
}

impl TaskSpec {
    pub fn validate_shape(&self) -> SwarmResult<()> {
        self.task_id.validate()?;
        self.worker_id.validate()?;
        validate_text(&self.title, "task title", MAX_TITLE_BYTES)?;
        validate_text(
            &self.instructions,
            "task instructions",
            MAX_INSTRUCTIONS_BYTES,
        )?;
        if self.dependencies.len() > MAX_DEPENDENCIES {
            return Err(SwarmError::bound(format!(
                "task '{}' declares more than {MAX_DEPENDENCIES} dependencies",
                self.task_id
            )));
        }
        let mut seen = BTreeSet::new();
        for dependency in &self.dependencies {
            dependency.validate()?;
            if dependency == &self.task_id {
                return Err(SwarmError::invalid(format!(
                    "task '{}' depends on itself",
                    self.task_id
                )));
            }
            if !seen.insert(dependency.clone()) {
                return Err(SwarmError::invalid(format!(
                    "task '{}' repeats a dependency",
                    self.task_id
                )));
            }
        }
        match (self.requires_computer_use, &self.computer_use) {
            (true, Some(requirement)) => requirement.validate()?,
            (true, None) => {
                return Err(SwarmError::capability(format!(
                    "task '{}' requires an explicit Computer Use authority binding",
                    self.task_id
                )));
            }
            (false, Some(_)) => {
                return Err(SwarmError::invalid(format!(
                    "task '{}' carries Computer Use authority but does not require it",
                    self.task_id
                )));
            }
            (false, None) => {}
        }
        match (&self.review_gate, self.kind) {
            (Some(gate), TaskKind::Synthesis) => gate.validate()?,
            (Some(_), _) => {
                return Err(SwarmError::invalid(format!(
                    "task '{}' declares a review gate but is not a synthesis task",
                    self.task_id
                )));
            }
            (None, TaskKind::Synthesis) => {
                return Err(SwarmError::invalid(format!(
                    "synthesis task '{}' must declare a review quorum",
                    self.task_id
                )));
            }
            (None, _) => {}
        }
        Ok(())
    }

    /// Check the node against the worker it names.
    pub(crate) fn validate_against_worker(&self, worker: &WorkerSpec) -> SwarmResult<()> {
        if !self.kind.accepts_role(worker.role) {
            return Err(SwarmError::invalid(format!(
                "task '{}' is assigned a worker whose role cannot perform that task kind",
                self.task_id
            )));
        }
        let required_capability = match self.kind {
            TaskKind::Work => WorkerCapability::ReadWorkspace,
            TaskKind::Review => WorkerCapability::Review,
            TaskKind::Synthesis => WorkerCapability::Synthesize,
        };
        if !worker.capabilities.contains(&required_capability) {
            return Err(SwarmError::capability(format!(
                "task '{}' requires worker capability '{}'",
                self.task_id,
                match required_capability {
                    WorkerCapability::ReadWorkspace => "read_workspace",
                    WorkerCapability::Review => "review",
                    WorkerCapability::Synthesize => "synthesize",
                    _ => unreachable!("task capability mapping is closed"),
                }
            )));
        }
        if self.requires_computer_use && !worker.allows_computer_use() {
            return Err(SwarmError::capability(format!(
                "task '{}' requires Computer Use but its worker holds no leased Computer Use capability",
                self.task_id
            )));
        }
        Ok(())
    }
}

/// The complete, validated description of a swarm campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwarmSpec {
    pub swarm_id: SwarmId,
    pub objective: String,
    pub catalog: ProviderCatalog,
    pub workers: Vec<WorkerSpec>,
    pub tasks: Vec<TaskSpec>,
    #[serde(default)]
    pub admission: AdmissionPolicy,
    #[serde(default)]
    pub budget: BudgetPolicy,
    #[serde(default)]
    pub failure: FailurePolicy,
}

impl SwarmSpec {
    /// Structural bounds that graph validation builds on.
    pub(crate) fn validate_bounds(&self) -> SwarmResult<()> {
        self.swarm_id.validate()?;
        validate_text(&self.objective, "objective", MAX_OBJECTIVE_BYTES)?;
        if self.tasks.is_empty() || self.tasks.len() > MAX_TASKS {
            return Err(SwarmError::bound(format!(
                "a swarm must hold between 1 and {MAX_TASKS} tasks"
            )));
        }
        if self.workers.is_empty() || self.workers.len() > MAX_WORKERS {
            return Err(SwarmError::bound(format!(
                "a swarm must hold between 1 and {MAX_WORKERS} workers"
            )));
        }
        self.admission.validate()?;
        self.budget.validate()?;
        self.catalog.validate()
    }

    /// Find a worker by identity.
    pub fn worker(&self, worker_id: &WorkerId) -> Option<&WorkerSpec> {
        self.workers.iter().find(|w| &w.worker_id == worker_id)
    }

    /// Find a task by identity.
    pub fn task(&self, task_id: &TaskId) -> Option<&TaskSpec> {
        self.tasks.iter().find(|t| &t.task_id == task_id)
    }
}
