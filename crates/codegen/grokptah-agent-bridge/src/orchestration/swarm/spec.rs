//! Declarative shape of one durable work graph.
//!
//! A spec is data the host validates once, before anything is dispatched. All
//! bounds are checked in a fixed order so a rejection is reproducible, and
//! cycle detection is iterative so an adversarial graph cannot exhaust the
//! stack.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::gateway_config::ComputerUseTier;
use crate::orchestration::types::{OrchError, OrchErrorCode, RunBounds};

use super::ids::{WorkId, WorkerId};

pub const WORK_GRAPH_SCHEMA_VERSION: u32 = 1;

/// Maximum work items in one graph. Matches the durable manager's per-plan
/// step ceiling so a graph cannot outgrow the coordinator that projects it.
pub const MAX_WORK_ITEMS: usize = 64;
/// Maximum distinct workers a graph may declare.
pub const MAX_WORKERS: usize = 32;
/// Maximum declared dependencies for one work item.
pub const MAX_DEPENDENCIES: usize = 16;
/// Maximum reviewers named by one quorum gate.
pub const MAX_REVIEWERS: usize = 16;
/// Maximum simultaneous in-flight dispatches a graph may request.
pub const MAX_IN_FLIGHT: usize = 16;
/// Maximum dispatch attempts across the whole graph lifetime.
pub const MAX_TOTAL_ATTEMPTS: u32 = 256;
/// Maximum bytes in any free-form spec string.
pub const MAX_TEXT_BYTES: usize = 4 * 1024;
/// Maximum wall-clock a graph may occupy.
pub const MAX_GRAPH_DURATION_MS: u64 = 12 * 60 * 60 * 1000;

/// What a worker is for. The set is closed: a role that is not listed cannot
/// be requested, however the spec was authored or deserialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    /// Reads only; produces findings.
    Investigate,
    /// Produces workspace changes.
    Build,
    /// Reviews another work item's result and must report a verdict.
    Review,
    /// Combines reviewed results behind a quorum gate.
    Synthesize,
}

/// Authority a work item may exercise.
///
/// There is deliberately no browser variant and no raw-host variant, so no
/// specification can express either. Browser and UI surfaces stay
/// authority-free by construction rather than by policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkCapability {
    ReadWorkspace,
    WriteWorkspace,
    ExecuteInWorktree,
    ComputerUse,
}

impl WorkCapability {
    /// True when holding this capability forces a dedicated worktree.
    pub fn requires_worktree(self) -> bool {
        matches!(self, Self::WriteWorkspace | Self::ExecuteInWorktree)
    }
}

/// How a work item's execution is isolated from the user's workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IsolationRequirement {
    /// Shares the session workspace. Only legal for strictly read-only work.
    #[default]
    Shared,
    /// A dedicated worktree is required before dispatch.
    DedicatedWorktree,
}

/// A reviewer quorum that gates a synthesis item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuorumGate {
    /// Reviewer work items whose verdicts are counted.
    pub reviewers: Vec<WorkId>,
    /// Approvals required. Never zero, never more than `reviewers.len()`.
    pub required_approvals: u32,
}

/// One exact provider/model pair a worker is permitted to use, with the
/// capabilities that pair has been *measured* to hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerBinding {
    pub provider_id: String,
    pub profile_id: String,
    pub model_id: String,
    pub effort: String,
    /// Highest Computer Use authority qualified for this exact pair.
    #[serde(default)]
    pub computer_use_tier: ComputerUseTier,
}

/// A declared worker. `credential_ref` is an opaque keychain reference; it is
/// never a secret and is never projected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerSpec {
    pub worker_id: WorkerId,
    pub role: WorkerRole,
    pub binding: WorkerBinding,
    /// Opaque keychain reference. Not a credential and not projected.
    pub credential_ref: String,
    /// The broadest capability set any work item on this worker may claim.
    pub capabilities: BTreeSet<WorkCapability>,
}

/// One node of the graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkSpec {
    pub work_id: WorkId,
    pub worker_id: WorkerId,
    /// Dispatch order tiebreak: higher runs first, then `work_id` ascending.
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub depends_on: Vec<WorkId>,
    /// Exact authority this item claims. Must be a subset of its worker's.
    pub capabilities: BTreeSet<WorkCapability>,
    #[serde(default)]
    pub isolation: IsolationRequirement,
    /// Required for a `Synthesize` item, rejected for every other role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum: Option<QuorumGate>,
    /// Per-item execution bounds, narrowed under the graph ceiling.
    pub bounds: RunBounds,
    /// Bounded, non-secret description of the item.
    pub objective: String,
}

/// Bounded budget the whole graph may consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphBudget {
    pub max_total_attempts: u32,
    pub max_in_flight: usize,
    pub max_wall_clock_ms: u64,
    /// Total provider tokens the graph may consume. Exhaustion stops
    /// admission; it never cancels work that is already running.
    pub max_total_tokens: u64,
}

impl Default for GraphBudget {
    fn default() -> Self {
        Self {
            max_total_attempts: 64,
            max_in_flight: 4,
            max_wall_clock_ms: 60 * 60 * 1000,
            max_total_tokens: 2_000_000,
        }
    }
}

impl GraphBudget {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.max_total_attempts == 0 || self.max_total_attempts > MAX_TOTAL_ATTEMPTS {
            return Err(invalid(format!(
                "max_total_attempts must be 1..={MAX_TOTAL_ATTEMPTS}"
            )));
        }
        if self.max_in_flight == 0 || self.max_in_flight > MAX_IN_FLIGHT {
            return Err(invalid(format!(
                "max_in_flight must be 1..={MAX_IN_FLIGHT}"
            )));
        }
        if self.max_wall_clock_ms == 0 || self.max_wall_clock_ms > MAX_GRAPH_DURATION_MS {
            return Err(invalid(format!(
                "max_wall_clock_ms must be 1..={MAX_GRAPH_DURATION_MS}"
            )));
        }
        if self.max_total_tokens == 0 {
            return Err(invalid("max_total_tokens must be > 0"));
        }
        Ok(())
    }
}

/// The full declaration of one graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkGraphSpec {
    pub schema_version: u32,
    /// Server ceiling every item's bounds must fit under.
    pub bounds_ceiling: RunBounds,
    pub budget: GraphBudget,
    pub workers: Vec<WorkerSpec>,
    pub work: Vec<WorkSpec>,
    /// What happens to independent branches when one item fails.
    #[serde(default)]
    pub failure_policy: FailurePolicy,
}

/// How a failure propagates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// Block transitive dependents; spare independent branches.
    #[default]
    BlockDependents,
    /// Stop admitting anything new across the whole graph.
    StopGraph,
}

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::InvalidRequest, message)
}

fn validate_text(value: &str, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.contains('\0') {
        return Err(invalid(format!(
            "{field} must be 1..={MAX_TEXT_BYTES} bytes and free of NUL"
        )));
    }
    Ok(())
}

impl WorkGraphSpec {
    /// Validate the whole declaration in a fixed order.
    ///
    /// Order matters: a graph that violates several rules always reports the
    /// same one, so a rejection is reproducible across runs and machines.
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != WORK_GRAPH_SCHEMA_VERSION {
            return Err(invalid("work graph schema version is not supported"));
        }
        self.bounds_ceiling.validate()?;
        self.budget.validate()?;
        self.validate_workers()?;
        self.validate_work_shape()?;
        let workers = self.worker_index();
        self.validate_work_against_workers(&workers)?;
        self.validate_dependencies()?;
        self.validate_quorum_gates()?;
        self.validate_acyclic()
    }

    fn validate_workers(&self) -> Result<(), OrchError> {
        if self.workers.is_empty() || self.workers.len() > MAX_WORKERS {
            return Err(invalid(format!(
                "a graph declares 1..={MAX_WORKERS} workers"
            )));
        }
        let mut seen = BTreeSet::new();
        for worker in &self.workers {
            worker.worker_id.validate()?;
            if !seen.insert(worker.worker_id.clone()) {
                return Err(invalid(format!("duplicate worker id {}", worker.worker_id)));
            }
            validate_text(&worker.binding.provider_id, "provider_id")?;
            validate_text(&worker.binding.profile_id, "profile_id")?;
            validate_text(&worker.binding.model_id, "model_id")?;
            validate_text(&worker.binding.effort, "effort")?;
            validate_text(&worker.credential_ref, "credential_ref")?;
            if worker.capabilities.is_empty() {
                return Err(invalid(format!(
                    "worker {} declares no capabilities",
                    worker.worker_id
                )));
            }
            if worker.capabilities.contains(&WorkCapability::ComputerUse)
                && !worker.binding.computer_use_tier.allows_observation()
            {
                return Err(invalid(format!(
                    "worker {} claims Computer Use without a qualified tier",
                    worker.worker_id
                )));
            }
        }
        Ok(())
    }

    fn validate_work_shape(&self) -> Result<(), OrchError> {
        if self.work.is_empty() || self.work.len() > MAX_WORK_ITEMS {
            return Err(invalid(format!(
                "a graph declares 1..={MAX_WORK_ITEMS} work items"
            )));
        }
        let mut seen = BTreeSet::new();
        for item in &self.work {
            item.work_id.validate()?;
            if !seen.insert(item.work_id.clone()) {
                return Err(invalid(format!("duplicate work id {}", item.work_id)));
            }
            validate_text(&item.objective, "objective")?;
            if item.depends_on.len() > MAX_DEPENDENCIES {
                return Err(invalid(format!(
                    "work {} declares more than {MAX_DEPENDENCIES} dependencies",
                    item.work_id
                )));
            }
            if item.capabilities.is_empty() {
                return Err(invalid(format!(
                    "work {} declares no capabilities",
                    item.work_id
                )));
            }
            let merged = super::spec::merge_item_bounds(&self.bounds_ceiling, &item.bounds)?;
            debug_assert_eq!(merged.max_rounds, item.bounds.max_rounds);
        }
        Ok(())
    }

    fn worker_index(&self) -> BTreeMap<&WorkerId, &WorkerSpec> {
        self.workers
            .iter()
            .map(|worker| (&worker.worker_id, worker))
            .collect()
    }

    fn validate_work_against_workers(
        &self,
        workers: &BTreeMap<&WorkerId, &WorkerSpec>,
    ) -> Result<(), OrchError> {
        for item in &self.work {
            let Some(worker) = workers.get(&item.worker_id) else {
                return Err(invalid(format!(
                    "work {} names unknown worker {}",
                    item.work_id, item.worker_id
                )));
            };
            if !item.capabilities.is_subset(&worker.capabilities) {
                return Err(invalid(format!(
                    "work {} claims authority beyond worker {}",
                    item.work_id, item.worker_id
                )));
            }
            let needs_worktree = item
                .capabilities
                .iter()
                .any(|capability| capability.requires_worktree());
            if needs_worktree && item.isolation != IsolationRequirement::DedicatedWorktree {
                return Err(invalid(format!(
                    "work {} writes or executes and must require a dedicated worktree",
                    item.work_id
                )));
            }
            match worker.role {
                WorkerRole::Investigate | WorkerRole::Review => {
                    if needs_worktree {
                        return Err(invalid(format!(
                            "work {} is read-only by role and cannot claim write or execute",
                            item.work_id
                        )));
                    }
                }
                WorkerRole::Build | WorkerRole::Synthesize => {}
            }
        }
        Ok(())
    }

    fn validate_dependencies(&self) -> Result<(), OrchError> {
        let declared: BTreeSet<&WorkId> = self.work.iter().map(|item| &item.work_id).collect();
        for item in &self.work {
            let mut seen = BTreeSet::new();
            for dependency in &item.depends_on {
                dependency.validate()?;
                if dependency == &item.work_id {
                    return Err(invalid(format!("work {} depends on itself", item.work_id)));
                }
                if !declared.contains(dependency) {
                    return Err(invalid(format!(
                        "work {} depends on unknown work {dependency}",
                        item.work_id
                    )));
                }
                if !seen.insert(dependency.clone()) {
                    return Err(invalid(format!(
                        "work {} declares duplicate dependency {dependency}",
                        item.work_id
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_quorum_gates(&self) -> Result<(), OrchError> {
        let roles: BTreeMap<&WorkId, WorkerRole> = self
            .work
            .iter()
            .filter_map(|item| {
                self.workers
                    .iter()
                    .find(|worker| worker.worker_id == item.worker_id)
                    .map(|worker| (&item.work_id, worker.role))
            })
            .collect();
        for item in &self.work {
            let role = roles.get(&item.work_id).copied();
            match (role, item.quorum.as_ref()) {
                (Some(WorkerRole::Synthesize), None) => {
                    return Err(invalid(format!(
                        "synthesis work {} must declare a quorum gate",
                        item.work_id
                    )));
                }
                (Some(WorkerRole::Synthesize), Some(gate)) => {
                    self.validate_gate(item, gate, &roles)?;
                }
                (_, Some(_)) => {
                    return Err(invalid(format!(
                        "work {} declares a quorum gate but is not a synthesis item",
                        item.work_id
                    )));
                }
                (_, None) => {}
            }
        }
        Ok(())
    }

    fn validate_gate(
        &self,
        item: &WorkSpec,
        gate: &QuorumGate,
        roles: &BTreeMap<&WorkId, WorkerRole>,
    ) -> Result<(), OrchError> {
        if gate.reviewers.is_empty() || gate.reviewers.len() > MAX_REVIEWERS {
            return Err(invalid(format!(
                "work {} names 1..={MAX_REVIEWERS} reviewers",
                item.work_id
            )));
        }
        if gate.required_approvals == 0 || gate.required_approvals as usize > gate.reviewers.len() {
            return Err(invalid(format!(
                "work {} requires 1..={} approvals",
                item.work_id,
                gate.reviewers.len()
            )));
        }
        let dependencies: BTreeSet<&WorkId> = item.depends_on.iter().collect();
        let mut seen = BTreeSet::new();
        for reviewer in &gate.reviewers {
            reviewer.validate()?;
            if !seen.insert(reviewer.clone()) {
                return Err(invalid(format!(
                    "work {} names reviewer {reviewer} twice",
                    item.work_id
                )));
            }
            if roles.get(reviewer).copied() != Some(WorkerRole::Review) {
                return Err(invalid(format!(
                    "work {} names {reviewer}, which is not a review item",
                    item.work_id
                )));
            }
            if !dependencies.contains(reviewer) {
                return Err(invalid(format!(
                    "work {} must depend on the reviewer {reviewer} it gates on",
                    item.work_id
                )));
            }
        }
        Ok(())
    }

    /// Iterative Kahn peel. A deep or adversarial graph cannot exhaust the
    /// stack, and the reported cycle members are deterministic.
    fn validate_acyclic(&self) -> Result<(), OrchError> {
        let mut indegree: BTreeMap<&WorkId, usize> = self
            .work
            .iter()
            .map(|item| (&item.work_id, item.depends_on.len()))
            .collect();
        let mut dependents: BTreeMap<&WorkId, Vec<&WorkId>> = BTreeMap::new();
        for item in &self.work {
            for dependency in &item.depends_on {
                dependents
                    .entry(dependency)
                    .or_default()
                    .push(&item.work_id);
            }
        }
        let mut ready: VecDeque<&WorkId> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut peeled = 0usize;
        while let Some(id) = ready.pop_front() {
            peeled += 1;
            for dependent in dependents.get(id).into_iter().flatten() {
                let degree = indegree.entry(dependent).or_insert(0);
                *degree = degree.saturating_sub(1);
                if *degree == 0 {
                    ready.push_back(dependent);
                }
            }
        }
        if peeled != self.work.len() {
            let mut remaining: Vec<&str> = indegree
                .iter()
                .filter(|(_, degree)| **degree > 0)
                .map(|(id, _)| id.as_str())
                .collect();
            remaining.sort_unstable();
            return Err(invalid(format!(
                "work graph contains a dependency cycle among [{}]",
                remaining.join(", ")
            )));
        }
        Ok(())
    }

    pub fn work_item(&self, work_id: &WorkId) -> Option<&WorkSpec> {
        self.work.iter().find(|item| &item.work_id == work_id)
    }

    pub fn worker(&self, worker_id: &WorkerId) -> Option<&WorkerSpec> {
        self.workers
            .iter()
            .find(|worker| &worker.worker_id == worker_id)
    }

    pub fn role_of(&self, work_id: &WorkId) -> Option<WorkerRole> {
        let item = self.work_item(work_id)?;
        self.worker(&item.worker_id).map(|worker| worker.role)
    }
}

/// A per-item bound may only narrow the graph ceiling, never widen it.
pub fn merge_item_bounds(ceiling: &RunBounds, item: &RunBounds) -> Result<RunBounds, OrchError> {
    item.validate()?;
    if item.max_prompt_bytes > ceiling.max_prompt_bytes
        || item.max_rounds > ceiling.max_rounds
        || item.max_duration_ms > ceiling.max_duration_ms
    {
        return Err(invalid("work bounds exceed the graph ceiling"));
    }
    Ok(item.clone())
}
