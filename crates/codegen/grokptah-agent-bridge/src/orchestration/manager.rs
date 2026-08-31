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
use super::types::hash_payload;
use super::workload::{AssignmentStatus, WorkDependency, WorkItem, WorkPolicy, WorkState};
use super::OrchError;

pub const MANAGER_SCHEMA_VERSION: u32 = 1;
pub const MAX_MANAGER_STEPS: usize = 64;
pub const MAX_MANAGER_IN_FLIGHT: u32 = 16;
pub const MAX_MANAGER_REPLANS: u32 = 16;
pub const MAX_MANAGER_DIRECTIVE_BYTES: usize = 16 * 1024;
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
    /// A failed historical step that an accepted replan explicitly replaced.
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ManagerCoordinationMode {
    #[default]
    Manual,
    Autonomous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManagerCoordinationPolicy {
    #[serde(default)]
    pub mode: ManagerCoordinationMode,
}

impl ManagerCoordinationPolicy {
    pub fn autonomous(&self) -> bool {
        self.mode == ManagerCoordinationMode::Autonomous
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerDecisionState {
    AwaitingResult,
    Proposed,
    Applied,
    Rejected,
    HumanRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerDecisionRecord {
    pub schema_version: u32,
    pub decision_id: String,
    pub plan_id: String,
    pub expected_plan_revision: u64,
    pub manager_agent_id: String,
    pub agent_spec_revision: u64,
    #[serde(default)]
    pub triggering_work_ids: Vec<String>,
    #[serde(default)]
    pub triggering_message_ids: Vec<String>,
    pub input_snapshot_hash: String,
    pub decision_work_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub state: ManagerDecisionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_directive: Option<ManagerDirectiveEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default)]
    pub applied_mutation_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ManagerDecisionRecord {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != MANAGER_SCHEMA_VERSION
            || self.expected_plan_revision == 0
            || self.agent_spec_revision == 0
        {
            return Err(invalid("manager decision schema or revision is invalid"));
        }
        if self.triggering_work_ids.len() > MAX_MANAGER_STEPS
            || self.triggering_message_ids.len() > MAX_MANAGER_STEPS
            || self.applied_mutation_ids.len() > MAX_MANAGER_STEPS
        {
            return Err(invalid("manager decision references exceed their bounds"));
        }
        for (value, field) in [
            (&self.decision_id, "decision_id"),
            (&self.plan_id, "plan_id"),
            (&self.manager_agent_id, "manager_agent_id"),
            (&self.input_snapshot_hash, "input_snapshot_hash"),
            (&self.decision_work_id, "decision_work_id"),
        ] {
            validate_id(value, field)?;
        }
        for id in self
            .triggering_work_ids
            .iter()
            .chain(self.triggering_message_ids.iter())
            .chain(self.applied_mutation_ids.iter())
        {
            validate_id(id, "manager decision reference")?;
        }
        if let Some(outcome) = &self.outcome {
            validate_text(outcome, "manager decision outcome")?;
        }
        if let Some(directive) = &self.proposed_directive {
            parse_manager_directive(
                &serde_json::to_string(directive)
                    .map_err(|error| invalid(format!("invalid stored directive: {error}")))?,
            )?;
        }
        Ok(())
    }
}

/// Strict proposal envelope emitted by a bounded manager-decision Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagerDirectiveEnvelope {
    pub schema_version: u32,
    pub occurrence_id: String,
    pub plan_id: String,
    pub expected_plan_revision: u64,
    pub manager_agent_id: String,
    pub expected_agent_spec_revision: u64,
    pub input_snapshot_hash: String,
    pub directive: ManagerDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagerDirective {
    #[serde(rename_all = "camelCase")]
    AppendReplacementSteps {
        reason: String,
        replaces_step_ids: Vec<String>,
        steps: Vec<ManagerStepSpec>,
    },
    #[serde(rename_all = "camelCase")]
    RequestOperatorIntervention { reason: String },
    #[serde(rename_all = "camelCase")]
    NoSafeAction { reason: String },
}

/// A JSON document whose objects contain no repeated key at any depth.
///
/// `serde_json::Value` silently collapses a duplicate key to last-wins. At
/// this boundary that would let one model response read one way to an auditor
/// and act another way through the applicator, so the envelope refuses the
/// document instead of choosing a winner.
struct UniqueKeyJson(Value);

impl<'de> Deserialize<'de> for UniqueKeyJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StrictValue;

        impl<'de> serde::de::Visitor<'de> for StrictValue {
            type Value = Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("JSON with unique object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
                Ok(Value::from(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
                Ok(Value::from(value))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
                Ok(Value::from(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.to_owned()))
            }

            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_none<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                UniqueKeyJson::deserialize(deserializer).map(|wrapped| wrapped.0)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(UniqueKeyJson(item)) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Value::Array(items))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut object = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    let UniqueKeyJson(value) = map.next_value()?;
                    if object.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate manager directive key `{key}`"
                        )));
                    }
                }
                Ok(Value::Object(object))
            }
        }

        deserializer.deserialize_any(StrictValue).map(UniqueKeyJson)
    }
}

pub fn parse_manager_directive(raw: &str) -> Result<ManagerDirectiveEnvelope, OrchError> {
    if raw.is_empty() || raw.len() > MAX_MANAGER_DIRECTIVE_BYTES {
        return Err(invalid("manager directive is empty or exceeds its bound"));
    }
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let UniqueKeyJson(value) = UniqueKeyJson::deserialize(&mut deserializer)
        .map_err(|error| invalid(format!("invalid manager directive: {error}")))?;
    deserializer
        .end()
        .map_err(|error| invalid(format!("invalid trailing manager output: {error}")))?;
    validate_directive_json_shape(&value)?;
    let envelope = serde_json::from_value::<ManagerDirectiveEnvelope>(value)
        .map_err(|error| invalid(format!("invalid manager directive: {error}")))?;
    // Revisions are one-based. Zero is never a real fence, and
    // `ManagerDecisionRecord::validate` already refuses it, so the envelope
    // parser refuses it too rather than leaving the downstream equality check
    // as the only thing standing between a zero fence and a plan mutation.
    if envelope.schema_version != MANAGER_SCHEMA_VERSION
        || envelope.expected_plan_revision == 0
        || envelope.expected_agent_spec_revision == 0
    {
        return Err(invalid("manager directive schema or revision is invalid"));
    }
    for (value, field) in [
        (&envelope.occurrence_id, "occurrence_id"),
        (&envelope.plan_id, "plan_id"),
        (&envelope.manager_agent_id, "manager_agent_id"),
        (&envelope.input_snapshot_hash, "input_snapshot_hash"),
    ] {
        validate_id(value, field)?;
    }
    match &envelope.directive {
        ManagerDirective::AppendReplacementSteps {
            reason,
            replaces_step_ids,
            steps,
        } => {
            validate_text(reason, "directive reason")?;
            if replaces_step_ids.is_empty() || replaces_step_ids.len() > MAX_MANAGER_STEPS {
                return Err(invalid(
                    "replacement directive must name bounded replaced steps",
                ));
            }
            for id in replaces_step_ids {
                validate_id(id, "replaces_step_id")?;
            }
            if steps.is_empty() || steps.len() > MAX_MANAGER_STEPS {
                return Err(invalid(
                    "replacement directive contains an invalid step count",
                ));
            }
            for step in steps {
                step.validate()?;
            }
        }
        ManagerDirective::RequestOperatorIntervention { reason }
        | ManagerDirective::NoSafeAction { reason } => validate_text(reason, "directive reason")?,
    }
    Ok(envelope)
}

fn validate_directive_json_shape(value: &Value) -> Result<(), OrchError> {
    fn only(
        object: &serde_json::Map<String, Value>,
        allowed: &[&str],
        at: &str,
    ) -> Result<(), OrchError> {
        if let Some(field) = object
            .keys()
            .find(|field| !allowed.contains(&field.as_str()))
        {
            return Err(invalid(format!(
                "unknown manager directive field `{at}.{field}`"
            )));
        }
        Ok(())
    }
    let root = value
        .as_object()
        .ok_or_else(|| invalid("manager directive must be an object"))?;
    only(
        root,
        &[
            "schemaVersion",
            "occurrenceId",
            "planId",
            "expectedPlanRevision",
            "managerAgentId",
            "expectedAgentSpecRevision",
            "inputSnapshotHash",
            "directive",
        ],
        "$",
    )?;
    let directive = root
        .get("directive")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("manager directive payload must be an object"))?;
    if directive.get("type").and_then(Value::as_str) != Some("append_replacement_steps") {
        return Ok(());
    }
    only(
        directive,
        &["type", "reason", "replacesStepIds", "steps"],
        "directive",
    )?;
    let steps = directive
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("replacement steps must be an array"))?;
    for (index, step) in steps.iter().enumerate() {
        let step = step
            .as_object()
            .ok_or_else(|| invalid("replacement step must be an object"))?;
        only(
            step,
            &[
                "stepId",
                "kind",
                "objective",
                "priority",
                "dependencies",
                "assignedAgentId",
                "policy",
            ],
            &format!("directive.steps[{index}]"),
        )?;
        if let Some(policy) = step.get("policy") {
            let policy = policy
                .as_object()
                .ok_or_else(|| invalid("replacement policy must be an object"))?;
            only(
                policy,
                &[
                    "bounds",
                    "retry",
                    "requiresApproval",
                    "maxConcurrentAttempts",
                    "managedExecution",
                ],
                &format!("directive.steps[{index}].policy"),
            )?;
            if let Some(bounds) = policy.get("bounds") {
                let bounds = bounds
                    .as_object()
                    .ok_or_else(|| invalid("replacement bounds must be an object"))?;
                only(
                    bounds,
                    &[
                        "maxPromptBytes",
                        "maxRounds",
                        "maxDurationMs",
                        "maxTotalTokens",
                    ],
                    &format!("directive.steps[{index}].policy.bounds"),
                )?;
            }
            if let Some(retry) = policy.get("retry") {
                let retry = retry
                    .as_object()
                    .ok_or_else(|| invalid("replacement retry policy must be an object"))?;
                only(
                    retry,
                    &["maxAttempts", "retryFailed", "retryExpired", "backoffMs"],
                    &format!("directive.steps[{index}].policy.retry"),
                )?;
            }
        }
    }
    Ok(())
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
        if self.step_id == "__manager_decision__" || self.kind == "manager-decision" {
            return Err(invalid("manager decision identifiers are reserved"));
        }
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
    /// Missing on v1 JSON and therefore safely defaults to manual operation.
    #[serde(default)]
    pub coordination: ManagerCoordinationPolicy,
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
            coordination: ManagerCoordinationPolicy::default(),
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
        let mut superseded = self
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    step.state,
                    ManagerStepState::Failed | ManagerStepState::Cancelled
                )
            })
            .map(|step| step.step_id.clone())
            .collect::<HashSet<_>>();
        loop {
            let before = superseded.len();
            for step in &self.steps {
                if !matches!(
                    step.state,
                    ManagerStepState::Succeeded | ManagerStepState::Superseded
                ) && step.dependencies.iter().any(|id| superseded.contains(id))
                {
                    superseded.insert(step.step_id.clone());
                }
            }
            if superseded.len() == before {
                break;
            }
        }
        for step in &mut self.steps {
            if superseded.contains(&step.step_id) {
                step.state = ManagerStepState::Superseded;
                step.updated_at = now;
            }
        }
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
        for item in work_items.iter().filter(|item| {
            item.session_id == self.session_id
                && item.workspace == self.workspace
                && item.parent_work_id.as_deref() == Some(self.root_work_id.as_str())
                && item.kind != "manager-decision"
                && item.source_manager_plan_id.as_deref() == Some(self.plan_id.as_str())
        }) {
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
            if step.state == ManagerStepState::Superseded {
                continue;
            }
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
        } else if self.steps.iter().all(|step| {
            matches!(
                step.state,
                ManagerStepState::Succeeded | ManagerStepState::Superseded
            )
        }) {
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

    pub fn replace_failed_steps(
        &mut self,
        reason: String,
        replaces_step_ids: &[String],
        steps: Vec<ManagerStepSpec>,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        let replace = replaces_step_ids.iter().cloned().collect::<HashSet<_>>();
        if replace.len() != replaces_step_ids.len() {
            return Err(invalid("replaced manager step IDs must be unique"));
        }
        let mut required = self
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    step.state,
                    ManagerStepState::Failed | ManagerStepState::Cancelled
                )
            })
            .map(|step| step.step_id.clone())
            .collect::<HashSet<_>>();
        loop {
            let before = required.len();
            for step in &self.steps {
                if !matches!(
                    step.state,
                    ManagerStepState::Succeeded | ManagerStepState::Superseded
                ) && step.dependencies.iter().any(|id| required.contains(id))
                {
                    required.insert(step.step_id.clone());
                }
            }
            if required.len() == before {
                break;
            }
        }
        if replace != required {
            return Err(invalid(
                "replacement must account for every failed step and blocked descendant",
            ));
        }
        for id in &replace {
            let step = self
                .steps
                .iter()
                .find(|step| &step.step_id == id)
                .ok_or_else(|| invalid("replacement references an unknown manager step"))?;
            if !required.contains(&step.step_id) {
                return Err(invalid(
                    "replacement may only supersede failed steps and blocked descendants",
                ));
            }
        }
        self.append_replan(reason, steps, now)?;
        Ok(())
    }

    pub fn manager_decision_snapshot(plan: &ManagerPlan, work_items: &[WorkItem]) -> Value {
        let mut selected = plan
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    step.state,
                    ManagerStepState::Failed | ManagerStepState::Cancelled
                )
            })
            .map(|step| step.step_id.as_str())
            .take(8)
            .collect::<HashSet<_>>();
        for step in plan.steps.iter().rev() {
            if selected.len() == 8 {
                break;
            }
            selected.insert(step.step_id.as_str());
        }
        let outcomes = plan
                .steps
                .iter()
                .filter(|step| selected.contains(step.step_id.as_str()))
                .filter_map(|step| {
                    let work = step
                        .work_id
                        .as_deref()
                        .and_then(|id| work_items.iter().find(|item| item.work_id == id));
                    work.map(|work| json!({
                "stepId": step.step_id,
                "workId": work.work_id,
                "workRevision": work.revision,
                "state": work_state_label(work.state),
                "summary": work.result.as_ref().map(|result| truncate_manager_text(&result.summary, 512)),
                "failure": work.result.as_ref().and_then(|result| result.failure.as_ref()).map(|failure| truncate_manager_text(failure, 512)),
            }))
                })
                .collect::<Vec<_>>();
        json!({
            "planId": plan.plan_id,
            "planRevision": plan.revision,
            "objective": truncate_manager_text(&plan.objective, 4096),
            "managerAgentId": plan.manager_agent_id,
            "outcomes": outcomes,
        })
    }

    pub fn manager_decision_id(plan: &ManagerPlan, snapshot: &Value) -> String {
        format!(
            "manager-decision-{}",
            &hash_payload(
                &json!({"planId": plan.plan_id, "revision": plan.revision, "snapshot": snapshot})
            )[..32]
        )
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

fn truncate_manager_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
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

    #[test]
    fn legacy_plan_json_defaults_to_manual_coordination() {
        let plan = ManagerPlan::new(
            Uuid::new_v4(),
            "/tmp/project",
            "manager",
            "objective",
            "root",
            vec![spec("a", &[])],
            1,
            2,
            Utc::now(),
        )
        .unwrap();
        let mut value = serde_json::to_value(plan).unwrap();
        value.as_object_mut().unwrap().remove("coordination");
        let recovered: ManagerPlan = serde_json::from_value(value).unwrap();
        assert!(!recovered.coordination.autonomous());
    }

    #[test]
    fn replacement_supersedes_failure_and_plan_can_succeed() {
        let now = Utc::now();
        let mut plan = ManagerPlan::new(
            Uuid::new_v4(),
            "/tmp/project",
            "manager",
            "objective",
            "root",
            vec![spec("original", &[])],
            1,
            2,
            now,
        )
        .unwrap();
        let mut original = plan.advance(&[], "operator", now).unwrap().remove(0);
        original.state = WorkState::Failed;
        original.result = Some(super::super::workload::WorkResult {
            summary: "failed".into(),
            evidence: vec![],
            artifacts: vec![],
            failure: Some("fixture".into()),
            cancellation_reason: None,
            completed_at: now,
        });
        original.bump_at(now);
        plan.advance(&[original.clone()], "operator", now).unwrap();
        assert_eq!(plan.state, ManagerPlanState::NeedsReplan);
        plan.replace_failed_steps(
            "replace failed path".into(),
            &["original".into()],
            vec![spec("replacement", &[])],
            now,
        )
        .unwrap();
        let mut replacement = plan
            .advance(&[original.clone()], "operator", now)
            .unwrap()
            .remove(0);
        replacement.state = WorkState::Succeeded;
        replacement.bump_at(now);
        plan.advance(&[original, replacement], "operator", now)
            .unwrap();
        assert_eq!(plan.steps[0].state, ManagerStepState::Superseded);
        assert_eq!(plan.state, ManagerPlanState::Succeeded);
    }

    #[test]
    fn replacement_must_cover_failed_step_descendants() {
        let now = Utc::now();
        let mut plan = ManagerPlan::new(
            Uuid::new_v4(),
            "/tmp/project",
            "manager",
            "objective",
            "root",
            vec![spec("a", &[]), spec("b", &["a"])],
            1,
            2,
            now,
        )
        .unwrap();
        let mut a = plan.advance(&[], "operator", now).unwrap().remove(0);
        a.state = WorkState::Failed;
        a.bump_at(now);
        plan.advance(&[a], "operator", now).unwrap();
        assert!(plan
            .replace_failed_steps(
                "incomplete replacement".into(),
                &["a".into()],
                vec![spec("a2", &[])],
                now,
            )
            .is_err());
        plan.replace_failed_steps(
            "replace blocked path".into(),
            &["a".into(), "b".into()],
            vec![spec("a2", &[]), spec("b2", &["a2"])],
            now,
        )
        .unwrap();
        assert!(plan.steps[..2]
            .iter()
            .all(|step| step.state == ManagerStepState::Superseded));
    }

    #[test]
    fn directive_parser_is_bounded_and_denies_unknown_fields() {
        let valid = json!({
            "schemaVersion": 1,
            "occurrenceId": "decision-1",
            "planId": "plan-1",
            "expectedPlanRevision": 3,
            "managerAgentId": "agent-1",
            "expectedAgentSpecRevision": 2,
            "inputSnapshotHash": "abc",
            "directive": {
                "type": "append_replacement_steps",
                "reason": "replace failure",
                "replacesStepIds": ["failed"],
                "steps": [{"stepId": "retry", "kind": "coding", "objective": "retry"}]
            }
        });
        parse_manager_directive(&valid.to_string()).unwrap();
        let mut unknown = valid;
        unknown["unexpected"] = json!(true);
        assert!(parse_manager_directive(&unknown.to_string()).is_err());
        let mut nested_unknown = unknown;
        nested_unknown.as_object_mut().unwrap().remove("unexpected");
        nested_unknown["directive"]["steps"][0]["policy"] = json!({
            "bounds": {
                "maxPromptBytes": 1024,
                "maxRounds": 1,
                "maxDurationMs": 1000,
                "unknownBound": 1
            }
        });
        assert!(parse_manager_directive(&nested_unknown.to_string()).is_err());
        assert!(parse_manager_directive("{} trailing").is_err());
        assert!(parse_manager_directive(&"x".repeat(MAX_MANAGER_DIRECTIVE_BYTES + 1)).is_err());
    }

    /// The directive envelope is the only place untrusted model output becomes
    /// a durable plan mutation, so every rejection below is a boundary
    /// guarantee rather than a formatting preference.
    #[test]
    fn directive_envelope_rejects_adversarial_model_output() {
        fn envelope(directive: Value) -> String {
            json!({
                "schemaVersion": 1,
                "occurrenceId": "decision-1",
                "planId": "plan-1",
                "expectedPlanRevision": 3,
                "managerAgentId": "agent-1",
                "expectedAgentSpecRevision": 2,
                "inputSnapshotHash": "abc",
                "directive": directive
            })
            .to_string()
        }
        fn no_safe_action() -> Value {
            json!({"type": "no_safe_action", "reason": "nothing safe"})
        }
        fn rejected(raw: &str, case: &str) {
            assert!(
                parse_manager_directive(raw).is_err(),
                "envelope must fail closed: {case}"
            );
        }

        // The three allowlisted directives are the whole vocabulary.
        parse_manager_directive(&envelope(no_safe_action())).unwrap();
        parse_manager_directive(&envelope(json!({
            "type": "request_operator_intervention",
            "reason": "needs a human"
        })))
        .unwrap();
        parse_manager_directive(&envelope(json!({
            "type": "append_replacement_steps",
            "reason": "replace failure",
            "replacesStepIds": ["failed"],
            "steps": [{"stepId": "retry", "kind": "coding", "objective": "retry"}]
        })))
        .unwrap();
        rejected(
            &envelope(json!({"type": "apply_directly", "reason": "just do it"})),
            "unknown directive type",
        );

        // Unknown fields are refused inside every variant, not only the one
        // the recursive shape check descends into.
        rejected(
            &envelope(json!({
                "type": "no_safe_action",
                "reason": "nothing safe",
                "smuggled": {"applyAnyway": true}
            })),
            "unknown field in a non-append directive",
        );
        rejected(
            &envelope(json!({
                "type": "request_operator_intervention",
                "reason": "needs a human",
                "replacesStepIds": ["failed"],
                "steps": [{"stepId": "sneaky", "kind": "coding", "objective": "sneak"}]
            })),
            "append payload smuggled into an intervention directive",
        );

        // Fences must be real. Revisions are one-based on both axes.
        for (field, value, case) in [
            ("expectedPlanRevision", json!(0), "zero plan revision"),
            ("expectedAgentSpecRevision", json!(0), "zero spec revision"),
            ("expectedPlanRevision", json!(-1), "negative plan revision"),
            (
                "expectedPlanRevision",
                json!(3.5),
                "fractional plan revision",
            ),
            ("schemaVersion", json!(2), "future schema version"),
        ] {
            let mut tampered: Value = serde_json::from_str(&envelope(no_safe_action())).unwrap();
            tampered[field] = value;
            rejected(&tampered.to_string(), case);
        }

        // Identity fields are bounded and path-safe, because they are used as
        // durable record keys.
        for (field, value, case) in [
            ("planId", json!("../../etc/passwd"), "plan id traversal"),
            ("occurrenceId", json!(""), "empty occurrence id"),
            ("managerAgentId", json!("a/b"), "agent id separator"),
            (
                "inputSnapshotHash",
                json!("x".repeat(MAX_MANAGER_ID_BYTES + 1)),
                "oversized snapshot hash",
            ),
        ] {
            let mut tampered: Value = serde_json::from_str(&envelope(no_safe_action())).unwrap();
            tampered[field] = value;
            rejected(&tampered.to_string(), case);
        }

        // Replacement graphs stay inside the plan's declared bounds.
        rejected(
            &envelope(json!({
                "type": "append_replacement_steps",
                "reason": "r",
                "replacesStepIds": [],
                "steps": [{"stepId": "retry", "kind": "coding", "objective": "retry"}]
            })),
            "replacement naming no superseded step",
        );
        rejected(
            &envelope(json!({
                "type": "append_replacement_steps",
                "reason": "r",
                "replacesStepIds": (0..=MAX_MANAGER_STEPS).map(|index| format!("s{index}")).collect::<Vec<_>>(),
                "steps": [{"stepId": "retry", "kind": "coding", "objective": "retry"}]
            })),
            "oversized replaced-step list",
        );
        rejected(
            &envelope(json!({
                "type": "append_replacement_steps",
                "reason": "r",
                "replacesStepIds": ["failed"],
                "steps": (0..=MAX_MANAGER_STEPS)
                    .map(|index| json!({"stepId": format!("s{index}"), "kind": "coding", "objective": "o"}))
                    .collect::<Vec<_>>()
            })),
            "oversized replacement step list",
        );
        rejected(
            &envelope(json!({
                "type": "append_replacement_steps",
                "reason": "r",
                "replacesStepIds": ["failed"],
                "steps": [{"stepId": "a", "kind": "coding", "objective": "o", "dependencies": ["a"]}]
            })),
            "self-dependent replacement step",
        );
        rejected(
            &envelope(json!({"type": "no_safe_action", "reason": ""})),
            "empty directive reason",
        );

        // Transport-level shapes that must never reach the typed envelope.
        for (raw, case) in [
            ("[]", "array root"),
            ("\"envelope\"", "string root"),
            ("null", "null root"),
            ("", "empty output"),
        ] {
            rejected(raw, case);
        }
        let mut depth_bomb = String::new();
        for _ in 0..2_000 {
            depth_bomb.push('[');
        }
        depth_bomb.push('1');
        for _ in 0..2_000 {
            depth_bomb.push(']');
        }
        rejected(&depth_bomb, "deeply nested output");
        rejected(
            r#"{"schemaVersion":1,"schemaVersion":1,"occurrenceId":"d","planId":"p","expectedPlanRevision":3,"managerAgentId":"a","expectedAgentSpecRevision":2,"inputSnapshotHash":"h","directive":{"type":"no_safe_action","reason":"r"}}"#,
            "duplicate envelope key",
        );
        rejected(
            r#"{"schemaVersion":1,"occurrenceId":"d","planId":"p","expectedPlanRevision":3,"managerAgentId":"a","expectedAgentSpecRevision":2,"inputSnapshotHash":"h","directive":{"type":"no_safe_action","reason":"audited","reason":"applied"}}"#,
            "duplicate key nested in the directive",
        );
        rejected(
            r#"{"schemaVersion":1,"occurrenceId":"d","planId":"p","expectedPlanRevision":3,"managerAgentId":"a","expectedAgentSpecRevision":2,"inputSnapshotHash":"h","directive":{"type":"append_replacement_steps","reason":"r","replacesStepIds":["failed"],"steps":[{"stepId":"audited","stepId":"applied","kind":"coding","objective":"o"}]}}"#,
            "duplicate key nested inside an array element",
        );
        rejected(
            &format!(
                "{} {}",
                envelope(no_safe_action()),
                envelope(no_safe_action())
            ),
            "two concatenated envelopes",
        );

        // Ordinary multi-line operator text stays legal; the bound is on size
        // and NUL, not on formatting.
        parse_manager_directive(&envelope(json!({
            "type": "no_safe_action",
            "reason": "line one\nline two\ttabbed"
        })))
        .unwrap();
        rejected(
            &envelope(json!({"type": "no_safe_action", "reason": "a\u{0}b"})),
            "NUL in directive reason",
        );
    }
}
