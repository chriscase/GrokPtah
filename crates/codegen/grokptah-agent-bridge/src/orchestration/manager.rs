//! Durable manager plans built on the existing Work ledger.
//!
//! A plan is coordination state, not a second queue. Each executable step is
//! materialized as an ordinary Work item and therefore uses the same leases,
//! assignments, native executor, reviews, and retention rules as operator
//! created work.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::message::MessageKind;
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
    AwaitingInput,
    AwaitingReview,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_notification_work_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_notification_message_id: Option<String>,
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
            last_notification_work_revision: None,
            last_notification_message_id: None,
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

/// A durable manager observation delivered through the existing message
/// ledger. It is a projection, not a second execution or inbox queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerNotification {
    pub step_id: String,
    pub work_id: String,
    pub work_revision: u64,
    pub kind: MessageKind,
    pub body: String,
    pub payload: Value,
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
            if let Some(revision) = step.last_notification_work_revision {
                if revision == 0 {
                    return Err(invalid(
                        "step.last_notification_work_revision must be positive",
                    ));
                }
            }
            if let Some(message_id) = &step.last_notification_message_id {
                validate_id(message_id, "step.last_notification_message_id")?;
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
                WorkState::AwaitingInput => ManagerStepState::AwaitingInput,
                WorkState::AwaitingApproval | WorkState::Review => ManagerStepState::AwaitingReview,
                WorkState::Blocked => ManagerStepState::Blocked,
                WorkState::Leased | WorkState::Running => ManagerStepState::InFlight,
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
                    ManagerStepState::Ready
                        | ManagerStepState::InFlight
                        | ManagerStepState::AwaitingInput
                        | ManagerStepState::AwaitingReview
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

    /// Return at most one notification for each step's current Work revision.
    /// The caller persists the resulting message ID back onto the plan after
    /// the message is durably accepted. This makes retries deterministic even
    /// after a process restart.
    pub fn pending_notifications(&self, work_items: &[WorkItem]) -> Vec<ManagerNotification> {
        let by_work_id = work_items
            .iter()
            .filter(|item| item.session_id == self.session_id && item.workspace == self.workspace)
            .map(|item| (item.work_id.as_str(), item))
            .collect::<HashMap<_, _>>();
        self.steps
            .iter()
            .filter_map(|step| {
                let work_id = step.work_id.as_deref()?;
                let work = by_work_id.get(work_id)?;
                if step.last_notification_work_revision == Some(work.revision) {
                    return None;
                }
                let kind = match work.state {
                    WorkState::AwaitingInput => MessageKind::Question,
                    WorkState::AwaitingApproval | WorkState::Review => MessageKind::ReviewRequest,
                    WorkState::Succeeded
                    | WorkState::Failed
                    | WorkState::Cancelled
                    | WorkState::Blocked => MessageKind::Status,
                    WorkState::Queued | WorkState::Leased | WorkState::Running => return None,
                };
                let state = work_state_label(work.state);
                let detail = work
                    .result
                    .as_ref()
                    .and_then(|result| result.failure.clone())
                    .or_else(|| work.blocked_reason.clone());
                let body = match kind {
                    MessageKind::Question => format!(
                        "Manager step {} is awaiting input for Work {}: {}",
                        step.step_id,
                        work.work_id,
                        detail
                            .as_deref()
                            .unwrap_or("the native worker needs a decision")
                    ),
                    MessageKind::ReviewRequest => format!(
                        "Manager step {} is awaiting review for Work {}",
                        step.step_id, work.work_id
                    ),
                    MessageKind::Status => format!(
                        "Manager step {} observed Work {} in state {}{}",
                        step.step_id,
                        work.work_id,
                        state,
                        detail
                            .as_deref()
                            .map(|value| format!(": {value}"))
                            .unwrap_or_default()
                    ),
                    _ => return None,
                };
                Some(ManagerNotification {
                    step_id: step.step_id.clone(),
                    work_id: work.work_id.clone(),
                    work_revision: work.revision,
                    kind,
                    body,
                    payload: json!({
                        "managerPlanId": self.plan_id,
                        "stepId": step.step_id,
                        "workId": work.work_id,
                        "workRevision": work.revision,
                        "workState": state,
                        "requiresManagerAction": matches!(
                            kind,
                            MessageKind::Question | MessageKind::ReviewRequest
                        ),
                    }),
                })
            })
            .collect()
    }

    /// Fence delivered observations onto the plan revision. Re-delivering an
    /// already recorded notification is idempotent; a different message for
    /// the same Work revision fails closed.
    pub fn mark_notifications_sent(
        &mut self,
        delivered: &[(String, u64, String)],
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        let mut changed = false;
        for (step_id, work_revision, message_id) in delivered {
            validate_id(step_id, "notification.step_id")?;
            validate_id(message_id, "notification.message_id")?;
            if *work_revision == 0 {
                return Err(invalid("notification.work_revision must be positive"));
            }
            let step = self
                .steps
                .iter_mut()
                .find(|step| step.step_id == *step_id)
                .ok_or_else(|| invalid("notification references an unknown manager step"))?;
            if step.last_notification_work_revision == Some(*work_revision) {
                if step.last_notification_message_id.as_deref() != Some(message_id) {
                    return Err(OrchError::new(
                        super::OrchErrorCode::Conflict,
                        "manager notification revision is already fenced by another message",
                    ));
                }
                continue;
            }
            step.last_notification_work_revision = Some(*work_revision);
            step.last_notification_message_id = Some(message_id.clone());
            step.updated_at = now;
            changed = true;
        }
        if changed {
            self.bump(now);
            self.validate()?;
        }
        Ok(())
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

fn work_state_label(state: WorkState) -> &'static str {
    match state {
        WorkState::Queued => "queued",
        WorkState::Leased => "leased",
        WorkState::Running => "running",
        WorkState::AwaitingInput => "awaiting_input",
        WorkState::AwaitingApproval => "awaiting_approval",
        WorkState::Review => "review",
        WorkState::Succeeded => "succeeded",
        WorkState::Failed => "failed",
        WorkState::Cancelled => "cancelled",
        WorkState::Blocked => "blocked",
    }
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

    #[test]
    fn notifications_distinguish_input_review_and_terminal_outcomes() {
        let now = Utc::now();
        let mut plan = ManagerPlan::new(
            Uuid::new_v4(),
            "/tmp/project",
            "manager",
            "objective",
            "root",
            vec![spec("a", &[])],
            1,
            2,
            now,
        )
        .unwrap();
        let created = plan.advance(&[], "operator", now).unwrap();
        let mut awaiting = created[0].clone();
        awaiting.state = WorkState::AwaitingInput;
        awaiting.blocked_reason = Some("permission required".into());
        awaiting.bump_at(now);
        plan.advance(&[awaiting.clone()], "operator", now).unwrap();
        assert_eq!(plan.steps[0].state, ManagerStepState::AwaitingInput);
        let question = plan.pending_notifications(&[awaiting.clone()]);
        assert_eq!(question.len(), 1);
        assert_eq!(question[0].kind, MessageKind::Question);
        plan.mark_notifications_sent(&[("a".into(), awaiting.revision, "message-1".into())], now)
            .unwrap();
        assert!(plan.pending_notifications(&[awaiting.clone()]).is_empty());

        let mut review = awaiting;
        review.state = WorkState::Review;
        review.bump_at(now + chrono::Duration::seconds(1));
        plan.advance(&[review.clone()], "operator", now).unwrap();
        assert_eq!(plan.steps[0].state, ManagerStepState::AwaitingReview);
        let review_request = plan.pending_notifications(&[review.clone()]);
        assert_eq!(review_request[0].kind, MessageKind::ReviewRequest);

        let mut failed = review;
        failed.state = WorkState::Failed;
        failed.result = Some(super::super::workload::WorkResult {
            summary: "failed".into(),
            failure: Some("fixture failure".into()),
            evidence: Vec::new(),
            artifacts: Vec::new(),
            cancellation_reason: None,
            completed_at: now,
        });
        failed.bump_at(now + chrono::Duration::seconds(2));
        plan.advance(&[failed.clone()], "operator", now).unwrap();
        assert_eq!(plan.state, ManagerPlanState::NeedsReplan);
        let status = plan.pending_notifications(&[failed]);
        assert_eq!(status[0].kind, MessageKind::Status);
        assert!(status[0].body.contains("fixture failure"));
    }
}
