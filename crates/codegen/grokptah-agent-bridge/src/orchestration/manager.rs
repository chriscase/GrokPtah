//! Durable manager plans built on the existing Work ledger.
//!
//! A plan is coordination state, not a second queue. Each executable step is
//! materialized as an ordinary Work item and therefore uses the same leases,
//! assignments, native executor, reviews, and retention rules as operator
//! created work.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::workload::{AssignmentStatus, WorkDependency, WorkItem, WorkPolicy, WorkState};
use super::OrchError;

pub const MANAGER_SCHEMA_VERSION: u32 = 1;
pub const MAX_MANAGER_STEPS: usize = 64;
pub const MAX_MANAGER_IN_FLIGHT: u32 = 16;
pub const MAX_MANAGER_REPLANS: u32 = 16;
const MAX_MANAGER_ID_BYTES: usize = 256;
const MAX_MANAGER_TEXT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerPlanState {
    Active,
    NeedsReplan,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerStepState {
    Pending,
    Ready,
    InFlight,
    Succeeded,
    Failed,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerStepSpec {
    pub step_id: String,
    pub kind: String,
    pub objective: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub assigned_agent_id: Option<String>,
    #[serde(default)]
    pub policy: WorkPolicy,
}

impl ManagerStepSpec {
    pub fn validate(&self) -> Result<(), OrchError> {
        validate_id(&self.step_id, "step_id")?;
        validate_text(&self.kind, "kind")?;
        validate_text(&self.objective, "objective")?;
        if self.dependencies.len() > MAX_MANAGER_STEPS {
            return Err(invalid("step dependencies exceed the manager bound"));
        }
        let mut dependencies = HashSet::new();
        for dependency in &self.dependencies {
            validate_id(dependency, "step dependency")?;
            if dependency == &self.step_id || !dependencies.insert(dependency) {
                return Err(invalid("step dependencies must be unique and acyclic"));
            }
        }
        if let Some(agent_id) = &self.assigned_agent_id {
            validate_id(agent_id, "assigned_agent_id")?;
        }
        self.policy.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerStep {
    pub step_id: String,
    pub kind: String,
    pub objective: String,
    pub priority: i32,
    pub dependencies: Vec<String>,
    pub assigned_agent_id: Option<String>,
    pub policy: WorkPolicy,
    pub state: ManagerStepState,
    pub work_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ManagerStep {
    fn from_spec(spec: ManagerStepSpec, now: DateTime<Utc>) -> Self {
        Self {
            step_id: spec.step_id,
            kind: spec.kind,
            objective: spec.objective,
            priority: spec.priority,
            dependencies: spec.dependencies,
            assigned_agent_id: spec.assigned_agent_id,
            policy: spec.policy,
            state: ManagerStepState::Pending,
            work_id: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn as_spec(&self) -> ManagerStepSpec {
        ManagerStepSpec {
            step_id: self.step_id.clone(),
            kind: self.kind.clone(),
            objective: self.objective.clone(),
            priority: self.priority,
            dependencies: self.dependencies.clone(),
            assigned_agent_id: self.assigned_agent_id.clone(),
            policy: self.policy.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub manager_agent_id: String,
    pub objective: String,
    pub root_work_id: String,
    pub revision: u64,
    pub state: ManagerPlanState,
    pub max_in_flight: u32,
    pub max_replans: u32,
    pub replan_count: u32,
    pub steps: Vec<ManagerStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ManagerPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: Uuid,
        workspace: impl Into<String>,
        manager_agent_id: impl Into<String>,
        objective: impl Into<String>,
        root_work_id: impl Into<String>,
        steps: Vec<ManagerStepSpec>,
        max_in_flight: u32,
        max_replans: u32,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        let plan = Self {
            schema_version: MANAGER_SCHEMA_VERSION,
            plan_id: Uuid::new_v4().to_string(),
            session_id,
            workspace: workspace.into(),
            manager_agent_id: manager_agent_id.into(),
            objective: objective.into(),
            root_work_id: root_work_id.into(),
            revision: 1,
            state: ManagerPlanState::Active,
            max_in_flight,
            max_replans,
            replan_count: 0,
            steps: steps
                .into_iter()
                .map(|step| ManagerStep::from_spec(step, now))
                .collect(),
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != MANAGER_SCHEMA_VERSION || self.revision == 0 {
            return Err(invalid(
                "manager plan schema version or revision is invalid",
            ));
        }
        validate_id(&self.plan_id, "plan_id")?;
        validate_id(&self.manager_agent_id, "manager_agent_id")?;
        validate_id(&self.root_work_id, "root_work_id")?;
        validate_text(&self.workspace, "workspace")?;
        validate_text(&self.objective, "objective")?;
        if self.max_in_flight == 0 || self.max_in_flight > MAX_MANAGER_IN_FLIGHT {
            return Err(invalid("max_in_flight exceeds the manager bound"));
        }
        if self.max_replans > MAX_MANAGER_REPLANS || self.replan_count > self.max_replans {
            return Err(invalid("replan count exceeds the manager bound"));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_MANAGER_STEPS {
            return Err(invalid("manager plan must contain between 1 and 64 steps"));
        }
        let mut ids = HashSet::new();
        let mut dependencies = HashMap::new();
        for step in &self.steps {
            let spec = step.as_spec();
            spec.validate()?;
            if !ids.insert(step.step_id.clone()) {
                return Err(invalid("manager step IDs must be unique"));
            }
            if step.state == ManagerStepState::Pending && step.work_id.is_some() {
                return Err(invalid("pending manager step cannot reference Work"));
            }
            if let Some(work_id) = &step.work_id {
                validate_id(work_id, "step.work_id")?;
            }
            if let Some(error) = &step.last_error {
                validate_text(error, "step.last_error")?;
            }
            dependencies.insert(step.step_id.clone(), step.dependencies.clone());
        }
        for step in &self.steps {
            for dependency in &step.dependencies {
                if !ids.contains(dependency) {
                    return Err(invalid("manager step references an unknown dependency"));
                }
            }
        }
        for step in &self.steps {
            let mut visiting = HashSet::new();
            assert_acyclic(
                &step.step_id,
                &dependencies,
                &mut visiting,
                &mut HashSet::new(),
            )?;
        }
        Ok(())
    }

    pub fn require_revision(&self, expected: Option<u64>) -> Result<(), OrchError> {
        if expected.is_some_and(|revision| revision != self.revision) {
            return Err(OrchError::new(
                super::OrchErrorCode::StaleVersion,
                "manager plan revision does not match expected_revision",
            ));
        }
        Ok(())
    }

    pub fn append_replan(
        &mut self,
        reason: String,
        steps: Vec<ManagerStepSpec>,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        validate_text(&reason, "replan reason")?;
        if self.state != ManagerPlanState::NeedsReplan {
            return Err(OrchError::new(
                super::OrchErrorCode::Conflict,
                "manager plan does not currently require re-planning",
            ));
        }
        if self.replan_count >= self.max_replans {
            return Err(OrchError::new(
                super::OrchErrorCode::Conflict,
                "manager plan has reached max_replans",
            ));
        }
        if steps.is_empty() || self.steps.len() + steps.len() > MAX_MANAGER_STEPS {
            return Err(invalid("replan would exceed the manager step bound"));
        }
        let mut existing = self
            .steps
            .iter()
            .map(|step| step.step_id.clone())
            .collect::<HashSet<_>>();
        for step in steps {
            step.validate()?;
            if !existing.insert(step.step_id.clone()) {
                return Err(invalid("replan step IDs must be new and unique"));
            }
            self.steps.push(ManagerStep::from_spec(step, now));
        }
        self.replan_count += 1;
        self.state = ManagerPlanState::Active;
        self.last_error = Some(reason);
        self.bump(now);
        self.validate()
    }

    pub fn advance(
        &mut self,
        work_items: &[WorkItem],
        actor_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<WorkItem>, OrchError> {
        if self.state != ManagerPlanState::Active {
            return Err(OrchError::new(
                super::OrchErrorCode::Conflict,
                "only an active manager plan can advance",
            ));
        }
        validate_text(actor_id, "actor_id")?;
        let by_work_id = work_items
            .iter()
            .filter(|item| item.session_id == self.session_id && item.workspace == self.workspace)
            .map(|item| (item.work_id.as_str(), item))
            .collect::<HashMap<_, _>>();
        let mut changed = false;
        let mut recovered_by_step = HashMap::new();
        for item in work_items
            .iter()
            .filter(|item| item.source_manager_plan_id.as_deref() == Some(self.plan_id.as_str()))
        {
            let Some(step_id) = item.source_manager_step_id.as_deref() else {
                continue;
            };
            if recovered_by_step
                .insert(step_id.to_string(), item.work_id.clone())
                .is_some()
            {
                return Err(OrchError::new(
                    super::OrchErrorCode::Conflict,
                    "manager plan has duplicate recovered Work for a step",
                ));
            }
        }
        for step in &mut self.steps {
            if step.work_id.is_none() {
                if let Some(work_id) = recovered_by_step.get(&step.step_id) {
                    step.work_id = Some(work_id.clone());
                    step.updated_at = now;
                    changed = true;
                }
            }
        }
        let mut failed = None;
        for step in &mut self.steps {
            let Some(work_id) = step.work_id.as_deref() else {
                continue;
            };
            let Some(work) = by_work_id.get(work_id) else {
                return Err(OrchError::new(
                    super::OrchErrorCode::Conflict,
                    "manager step references missing Work",
                ));
            };
            let next = match work.state {
                WorkState::Succeeded => ManagerStepState::Succeeded,
                WorkState::Failed => ManagerStepState::Failed,
                WorkState::Cancelled => ManagerStepState::Cancelled,
                WorkState::Queued => ManagerStepState::Ready,
                WorkState::Blocked => ManagerStepState::Blocked,
                WorkState::Leased
                | WorkState::Running
                | WorkState::AwaitingInput
                | WorkState::AwaitingApproval
                | WorkState::Review => ManagerStepState::InFlight,
            };
            if step.state != next {
                step.state = next;
                step.updated_at = now;
                changed = true;
            }
            if matches!(next, ManagerStepState::Failed | ManagerStepState::Cancelled)
                && step.last_error.is_none()
            {
                let error = work
                    .result
                    .as_ref()
                    .and_then(|result| result.failure.clone())
                    .unwrap_or_else(|| {
                        if next == ManagerStepState::Cancelled {
                            "step Work was cancelled".into()
                        } else {
                            "step Work failed".into()
                        }
                    });
                step.last_error = Some(error.clone());
                failed = Some(error);
            }
        }
        if let Some(error) = failed {
            self.state = ManagerPlanState::NeedsReplan;
            self.last_error = Some(error);
            changed = true;
        } else if self
            .steps
            .iter()
            .all(|step| step.state == ManagerStepState::Succeeded)
        {
            self.state = ManagerPlanState::Succeeded;
            self.last_error = None;
            changed = true;
        }
        if self.state != ManagerPlanState::Active {
            if changed {
                self.bump(now);
                self.validate()?;
            }
            return Ok(Vec::new());
        }

        let active = self
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    step.state,
                    ManagerStepState::Ready | ManagerStepState::InFlight
                )
            })
            .count() as u32;
        let mut created = Vec::new();
        let step_states = self
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), step.state))
            .collect::<HashMap<_, _>>();
        let step_work_ids = self
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), step.work_id.clone()))
            .collect::<HashMap<_, _>>();
        let mut capacity = self.max_in_flight.saturating_sub(active);
        for step in &mut self.steps {
            if capacity == 0 || step.state != ManagerStepState::Pending {
                continue;
            }
            let ready = step.dependencies.iter().all(|dependency| {
                step_states.get(dependency) == Some(&ManagerStepState::Succeeded)
                    && step_work_ids
                        .get(dependency)
                        .and_then(|work_id| work_id.as_deref())
                        .is_some()
            });
            if !ready {
                continue;
            }
            let dependencies = step
                .dependencies
                .iter()
                .filter_map(|dependency| {
                    step_work_ids
                        .get(dependency.as_str())
                        .and_then(|work_id| work_id.as_ref())
                        .map(|work_id| WorkDependency {
                            work_id: work_id.clone(),
                            required_state: WorkState::Succeeded,
                        })
                })
                .collect::<Vec<_>>();
            let mut work = WorkItem::new_at(
                step.kind.clone(),
                step.objective.clone(),
                self.session_id,
                self.workspace.clone(),
                actor_id,
                step.policy.clone(),
                now,
            )?;
            work.priority = step.priority;
            work.parent_work_id = Some(self.root_work_id.clone());
            work.dependencies = dependencies;
            work.source_manager_plan_id = Some(self.plan_id.clone());
            work.source_manager_step_id = Some(step.step_id.clone());
            if let Some(agent_id) = &step.assigned_agent_id {
                work.assigned_agent_id = Some(agent_id.clone());
                work.assignment_status = AssignmentStatus::Accepted;
            }
            work.validate()?;
            step.work_id = Some(work.work_id.clone());
            step.state = ManagerStepState::Ready;
            step.updated_at = now;
            created.push(work);
            capacity -= 1;
            changed = true;
        }
        if changed {
            self.bump(now);
            self.validate()?;
        }
        Ok(created)
    }

    fn bump(&mut self, now: DateTime<Utc>) {
        self.revision = self.revision.saturating_add(1);
        self.updated_at = now;
    }
}

fn assert_acyclic(
    id: &str,
    dependencies: &HashMap<String, Vec<String>>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) -> Result<(), OrchError> {
    if visited.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id.to_string()) {
        return Err(invalid("manager step dependencies contain a cycle"));
    }
    for dependency in dependencies.get(id).into_iter().flatten() {
        assert_acyclic(dependency, dependencies, visiting, visited)?;
    }
    visiting.remove(id);
    visited.insert(id.to_string());
    Ok(())
}

fn validate_id(value: &str, field: &str) -> Result<(), OrchError> {
    if value.is_empty()
        || value.len() > MAX_MANAGER_ID_BYTES
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || value.contains('\0')
    {
        return Err(invalid(format!("{field} is empty or invalid")));
    }
    Ok(())
}

fn validate_text(value: &str, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > MAX_MANAGER_TEXT_BYTES || value.contains('\0') {
        return Err(invalid(format!("{field} is empty or exceeds its bound")));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> OrchError {
    OrchError::new(super::OrchErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(step_id: &str, dependencies: &[&str]) -> ManagerStepSpec {
        ManagerStepSpec {
            step_id: step_id.into(),
            kind: "coding".into(),
            objective: format!("do {step_id}"),
            priority: 0,
            dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
            assigned_agent_id: None,
            policy: WorkPolicy::default(),
        }
    }

    #[test]
    fn rejects_cycles_and_unknown_dependencies() {
        let now = Utc::now();
        let error = ManagerPlan::new(
            Uuid::new_v4(),
            "/tmp/project",
            "manager",
            "objective",
            "root",
            vec![spec("a", &["b"]), spec("b", &["a"])],
            2,
            2,
            now,
        )
        .expect_err("cycle must fail closed");
        assert!(error.message.contains("cycle"));

        let error = ManagerPlan::new(
            Uuid::new_v4(),
            "/tmp/project",
            "manager",
            "objective",
            "root",
            vec![spec("a", &["missing"])],
            2,
            2,
            now,
        )
        .expect_err("unknown dependency must fail closed");
        assert!(error.message.contains("unknown dependency"));
    }

    #[test]
    fn advance_materializes_ready_steps_and_unlocks_dependents() {
        let now = Utc::now();
        let mut plan = ManagerPlan::new(
            Uuid::new_v4(),
            "/tmp/project",
            "manager",
            "objective",
            "root",
            vec![spec("a", &[]), spec("b", &["a"])],
            2,
            2,
            now,
        )
        .unwrap();
        let first = plan.advance(&[], "operator", now).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(plan.steps[0].state, ManagerStepState::Ready);
        let second = plan.advance(&first, "operator", now).unwrap();
        assert!(second.is_empty(), "a queued step still counts as in flight");

        let mut completed = first[0].clone();
        completed.state = WorkState::Succeeded;
        let third = plan.advance(&[completed], "operator", now).unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(plan.steps[1].state, ManagerStepState::Ready);
        assert_eq!(third[0].dependencies[0].work_id, first[0].work_id);
    }
}
