//! Bounded, restart-safe planning for the enterprise review lane.
//!
//! This module deliberately stops at a provider-neutral execution contract:
//! it creates deterministic specialized passes, accepts only secret-free
//! finding references, and persists replay-safe checkpoints.  Provider calls,
//! source reads, and publication remain outside this module and must be
//! supplied by an authorized worker.  That separation is what prevents a
//! modest company gateway from silently gaining a stronger fallback route or
//! a write-capable review surface.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::enterprise_review::{
    admit_enterprise_review, EnterpriseReviewAdmissionError, EnterpriseReviewEvidence,
    EnterpriseReviewLease, EnterpriseReviewPolicy,
};

pub const ENTERPRISE_REVIEW_PLAN_SCHEMA: &str = "grokptah.enterprise-review-plan.v1";
pub const ENTERPRISE_REVIEW_CHECKPOINT_SCHEMA: &str = "grokptah.enterprise-review-checkpoint.v1";
pub const ENTERPRISE_REVIEW_OUTCOME_SCHEMA: &str = "grokptah.enterprise-review-outcome.v1";
pub const ENTERPRISE_REVIEW_WORK_PLAN_SCHEMA: &str = "grokptah.enterprise-review-work-plan.v1";
pub const MAX_ENTERPRISE_REVIEW_PASSES: usize = 7;
pub const MAX_ENTERPRISE_REVIEW_PASS_ATTEMPTS: u32 = 3;
pub const MAX_ENTERPRISE_REVIEW_FINDINGS_PER_PASS: usize = 256;
pub const MAX_ENTERPRISE_REVIEW_LOCATION_BYTES: usize = 512;

/// Derive the request id a host broker should use when materializing one
/// projected pass. The plan and work key make retries independent of a
/// transient UI/request id while remaining bound to one exact plan.
pub fn enterprise_review_work_request_id(plan_digest: &str, work_key: &str) -> String {
    format!("enterprise-review:{plan_digest}:{work_key}")
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EnterpriseReviewPassKind {
    Correctness,
    Security,
    Concurrency,
    Performance,
    Tests,
    Api,
    UserExperience,
}

impl EnterpriseReviewPassKind {
    pub const ALL: [Self; MAX_ENTERPRISE_REVIEW_PASSES] = [
        Self::Correctness,
        Self::Security,
        Self::Concurrency,
        Self::Performance,
        Self::Tests,
        Self::Api,
        Self::UserExperience,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::Security => "security",
            Self::Concurrency => "concurrency",
            Self::Performance => "performance",
            Self::Tests => "tests",
            Self::Api => "api",
            Self::UserExperience => "user_experience",
        }
    }

    pub const fn objective(self) -> &'static str {
        match self {
            Self::Correctness => {
                "Trace changed behavior and cross-file dataflow to concrete defects."
            }
            Self::Security => {
                "Check trust boundaries, secrets, authorization, injection, and egress."
            }
            Self::Concurrency => {
                "Check races, restart recovery, idempotency, and ownership fences."
            }
            Self::Performance => {
                "Check resource bounds, hot paths, backpressure, and unbounded work."
            }
            Self::Tests => {
                "Check test coverage, missing adversarial cases, and false-positive risk."
            }
            Self::Api => "Check public contracts, compatibility, schemas, and migration behavior.",
            Self::UserExperience => {
                "Check operator workflow, accessibility, errors, and explainability."
            }
        }
    }
}

pub const ENTERPRISE_REVIEW_PASS_ATTEMPTS: u32 = MAX_ENTERPRISE_REVIEW_PASS_ATTEMPTS;
pub const ENTERPRISE_REVIEW_PASS_KINDS: [EnterpriseReviewPassKind; MAX_ENTERPRISE_REVIEW_PASSES] =
    EnterpriseReviewPassKind::ALL;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewPass {
    pub pass_id: String,
    pub kind: EnterpriseReviewPassKind,
    pub objective_digest: String,
    pub request_budget: u32,
    pub token_budget: u64,
    pub duration_budget_ms: u64,
}

impl EnterpriseReviewPass {
    /// Project one specialist into the provider-neutral durable worker
    /// contract. The objective contains only opaque digests; the worker must
    /// obtain source through its separately authorized, read-only workspace
    /// scope rather than receiving a route, URL, or raw prompt here.
    pub fn work_template(&self) -> crate::orchestration::WorkTemplate {
        crate::orchestration::WorkTemplate {
            kind: format!("enterprise_review:{}", self.kind.id()),
            objective: format!(
                "Run bounded {} review pass; objective={}, source and route are resolved by the admitted worker scope.",
                self.kind.id(), self.objective_digest
            ),
            priority: 0,
            policy: crate::orchestration::WorkPolicy {
                bounds: crate::orchestration::RunBounds {
                    max_prompt_bytes: 32 * 1024,
                    max_rounds: self.request_budget,
                    max_duration_ms: self.duration_budget_ms,
                    max_total_tokens: Some(self.token_budget),
                },
                retry: crate::orchestration::WorkRetryPolicy {
                    max_attempts: MAX_ENTERPRISE_REVIEW_PASS_ATTEMPTS,
                    retry_failed: true,
                    retry_expired: true,
                    backoff_ms: 0,
                },
                ..crate::orchestration::WorkPolicy::default()
            },
        }
    }
}

/// A deterministic, provider-neutral durable-work projection for one
/// specialist pass. `work_key` is stable across retries and process restarts;
/// it is not the randomly assigned WorkItem UUID and contains no credential,
/// endpoint, prompt, or source contents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewWorkItemTemplate {
    pub schema: String,
    pub work_key: String,
    pub review_id: String,
    pub pass_id: String,
    pub kind: EnterpriseReviewPassKind,
    pub objective_digest: String,
    pub template: crate::orchestration::WorkTemplate,
}

/// The immutable work graph a host broker may materialize into durable
/// WorkItems. Passes are intentionally independent and may run in parallel;
/// final aggregation remains bound to the review checkpoint and exact plan
/// digest rather than to provider-specific worker state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewWorkPlan {
    pub schema: String,
    pub review_id: String,
    pub plan_digest: String,
    pub admission: EnterpriseReviewEvidence,
    pub repository_fingerprint: String,
    pub scope_fingerprint: String,
    pub work_items: Vec<EnterpriseReviewWorkItemTemplate>,
    pub work_plan_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewPlan {
    pub schema: String,
    pub review_id: String,
    pub repository_fingerprint: String,
    pub scope_fingerprint: String,
    pub admission: EnterpriseReviewEvidence,
    pub passes: Vec<EnterpriseReviewPass>,
    pub max_requests: u32,
    pub max_tokens: u64,
    pub max_duration_ms: u64,
    pub plan_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnterpriseReviewPassStatus {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewFindingRef {
    pub finding_fingerprint: String,
    pub location: String,
    pub line_start: u32,
    pub line_end: u32,
    pub category: String,
    pub confidence_bps: u16,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewPassResult {
    pub schema: String,
    pub pass_id: String,
    pub attempt: u32,
    pub status: EnterpriseReviewPassStatus,
    pub requests: u32,
    pub tokens: u64,
    pub duration_ms: u64,
    pub findings: Vec<EnterpriseReviewFindingRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewCheckpoint {
    pub schema: String,
    pub review_id: String,
    pub plan_digest: String,
    pub revision: u64,
    /// All accepted attempts, including interrupted/failed attempts. Keeping
    /// history makes retry cost auditable after a process restart.
    pub results: Vec<EnterpriseReviewPassResult>,
    pub requests_used: u32,
    pub tokens_used: u64,
    pub duration_used_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterpriseReviewOutcome {
    pub schema: String,
    pub review_id: String,
    pub plan_digest: String,
    pub completed_passes: usize,
    pub unique_findings: usize,
    pub confirmed_findings: usize,
    pub requests_used: u32,
    pub tokens_used: u64,
    pub duration_used_ms: u64,
    pub read_only: bool,
    pub network_egress: bool,
    pub workspace_mutated: bool,
    pub secret_free: bool,
    pub quality_claim_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterpriseReviewPlanError {
    Admission(EnterpriseReviewAdmissionError),
    InvalidField(&'static str),
    InvalidPass(&'static str),
    DuplicatePass,
    BudgetExceeded(&'static str),
    PlanMismatch,
    CheckpointInvalid(&'static str),
    NotComplete,
}

impl std::fmt::Display for EnterpriseReviewPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(error) => error.fmt(f),
            Self::InvalidField(name) => write!(f, "invalid enterprise review plan field: {name}"),
            Self::InvalidPass(name) => write!(f, "invalid enterprise review pass: {name}"),
            Self::DuplicatePass => write!(f, "enterprise review pass is already recorded"),
            Self::BudgetExceeded(name) => {
                write!(f, "enterprise review plan budget exceeded: {name}")
            }
            Self::PlanMismatch => write!(f, "enterprise review checkpoint does not match plan"),
            Self::CheckpointInvalid(name) => {
                write!(f, "invalid enterprise review checkpoint: {name}")
            }
            Self::NotComplete => write!(f, "enterprise review is not complete"),
        }
    }
}

impl std::error::Error for EnterpriseReviewPlanError {}

impl From<EnterpriseReviewAdmissionError> for EnterpriseReviewPlanError {
    fn from(error: EnterpriseReviewAdmissionError) -> Self {
        Self::Admission(error)
    }
}

/// Build the exact seven-pass decomposition after the route has been admitted.
/// The returned plan is secret-free and deterministic for the same inputs.
pub fn build_enterprise_review_plan(
    lease: &EnterpriseReviewLease,
    policy: &EnterpriseReviewPolicy,
    now: DateTime<Utc>,
    review_id: impl Into<String>,
    repository_fingerprint: impl Into<String>,
    scope_fingerprint: impl Into<String>,
) -> Result<EnterpriseReviewPlan, EnterpriseReviewPlanError> {
    let admission = admit_enterprise_review(lease, policy, now)?;
    let review_id = review_id.into();
    let repository_fingerprint = repository_fingerprint.into();
    let scope_fingerprint = scope_fingerprint.into();
    if !valid_opaque_id(&review_id) {
        return Err(EnterpriseReviewPlanError::InvalidField("review_id"));
    }
    if !valid_fingerprint(&repository_fingerprint) {
        return Err(EnterpriseReviewPlanError::InvalidField(
            "repository_fingerprint",
        ));
    }
    if !valid_fingerprint(&scope_fingerprint) {
        return Err(EnterpriseReviewPlanError::InvalidField("scope_fingerprint"));
    }

    let request_budget = (policy.max_requests / MAX_ENTERPRISE_REVIEW_PASSES as u32).max(1);
    let token_budget = (policy.max_tokens / MAX_ENTERPRISE_REVIEW_PASSES as u64).max(1);
    let duration_budget = (policy.max_duration_ms / MAX_ENTERPRISE_REVIEW_PASSES as u64).max(1);
    let passes = EnterpriseReviewPassKind::ALL
        .into_iter()
        .map(|kind| EnterpriseReviewPass {
            pass_id: format!("{review_id}:{}", kind.id()),
            kind,
            objective_digest: digest(&format!(
                "{}|{}|{}|{}|{}",
                review_id,
                kind.id(),
                repository_fingerprint,
                scope_fingerprint,
                admission.route_binding_digest
            )),
            request_budget,
            token_budget,
            duration_budget_ms: duration_budget,
        })
        .collect::<Vec<_>>();
    let mut plan = EnterpriseReviewPlan {
        schema: ENTERPRISE_REVIEW_PLAN_SCHEMA.to_owned(),
        review_id,
        repository_fingerprint,
        scope_fingerprint,
        admission,
        passes,
        max_requests: policy.max_requests,
        max_tokens: policy.max_tokens,
        max_duration_ms: policy.max_duration_ms,
        plan_digest: String::new(),
    };
    plan.plan_digest = plan_digest(&plan);
    Ok(plan)
}

impl EnterpriseReviewPlan {
    /// Project this admitted plan into stable durable-worker intents. The
    /// projection is deliberately side-effect free: a host must still issue
    /// the worker credential, bind the workspace scope, and persist each
    /// resulting WorkItem through its authorized orchestration service.
    pub fn work_plan(&self) -> Result<EnterpriseReviewWorkPlan, EnterpriseReviewPlanError> {
        self.validate()?;
        let work_items = self
            .passes
            .iter()
            .map(|pass| {
                let template = pass.work_template();
                template
                    .validate()
                    .map_err(|_| EnterpriseReviewPlanError::InvalidField("work_template"))?;
                Ok(EnterpriseReviewWorkItemTemplate {
                    schema: ENTERPRISE_REVIEW_WORK_PLAN_SCHEMA.to_owned(),
                    work_key: digest(&format!(
                        "{}|{}|{}",
                        self.plan_digest, pass.pass_id, pass.objective_digest
                    )),
                    review_id: self.review_id.clone(),
                    pass_id: pass.pass_id.clone(),
                    kind: pass.kind,
                    objective_digest: pass.objective_digest.clone(),
                    template,
                })
            })
            .collect::<Result<Vec<_>, EnterpriseReviewPlanError>>()?;
        let mut work_plan = EnterpriseReviewWorkPlan {
            schema: ENTERPRISE_REVIEW_WORK_PLAN_SCHEMA.to_owned(),
            review_id: self.review_id.clone(),
            plan_digest: self.plan_digest.clone(),
            admission: self.admission.clone(),
            repository_fingerprint: self.repository_fingerprint.clone(),
            scope_fingerprint: self.scope_fingerprint.clone(),
            work_items,
            work_plan_digest: String::new(),
        };
        work_plan.work_plan_digest = work_plan_digest(&work_plan);
        work_plan.validate()?;
        Ok(work_plan)
    }

    fn pass(&self, pass_id: &str) -> Result<&EnterpriseReviewPass, EnterpriseReviewPlanError> {
        self.passes
            .iter()
            .find(|pass| pass.pass_id == pass_id)
            .ok_or(EnterpriseReviewPlanError::InvalidPass("unknown pass_id"))
    }

    fn validate(&self) -> Result<(), EnterpriseReviewPlanError> {
        if self.schema != ENTERPRISE_REVIEW_PLAN_SCHEMA
            || self.passes.len() != MAX_ENTERPRISE_REVIEW_PASSES
            || self.plan_digest != plan_digest(self)
        {
            return Err(EnterpriseReviewPlanError::PlanMismatch);
        }
        self.admission
            .validate()
            .map_err(|_| EnterpriseReviewPlanError::InvalidField("admission"))?;
        let mut ids = BTreeSet::new();
        let mut requests = 0u32;
        let mut tokens = 0u64;
        let mut duration = 0u64;
        for pass in &self.passes {
            if !ids.insert(pass.pass_id.clone()) {
                return Err(EnterpriseReviewPlanError::DuplicatePass);
            }
            if pass.pass_id != format!("{}:{}", self.review_id, pass.kind.id())
                || !valid_fingerprint(&pass.objective_digest)
                || pass.request_budget == 0
                || pass.token_budget == 0
                || pass.duration_budget_ms == 0
            {
                return Err(EnterpriseReviewPlanError::InvalidPass("shape"));
            }
            requests = requests.saturating_add(pass.request_budget);
            tokens = tokens.saturating_add(pass.token_budget);
            duration = duration.saturating_add(pass.duration_budget_ms);
        }
        if requests > self.max_requests {
            return Err(EnterpriseReviewPlanError::BudgetExceeded("requests"));
        }
        if tokens > self.max_tokens {
            return Err(EnterpriseReviewPlanError::BudgetExceeded("tokens"));
        }
        if duration > self.max_duration_ms {
            return Err(EnterpriseReviewPlanError::BudgetExceeded("duration_ms"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct EnterpriseReviewRun {
    plan: EnterpriseReviewPlan,
    results: BTreeMap<String, EnterpriseReviewPassResult>,
    history: Vec<EnterpriseReviewPassResult>,
    revision: u64,
    requests_used: u32,
    tokens_used: u64,
    duration_used_ms: u64,
}

impl EnterpriseReviewRun {
    pub fn start(plan: EnterpriseReviewPlan) -> Result<Self, EnterpriseReviewPlanError> {
        plan.validate()?;
        Ok(Self {
            plan,
            results: BTreeMap::new(),
            history: Vec::new(),
            revision: 1,
            requests_used: 0,
            tokens_used: 0,
            duration_used_ms: 0,
        })
    }

    pub fn plan(&self) -> &EnterpriseReviewPlan {
        &self.plan
    }

    pub fn record_pass(
        &mut self,
        mut result: EnterpriseReviewPassResult,
    ) -> Result<(), EnterpriseReviewPlanError> {
        let pass = self.plan.pass(&result.pass_id)?;
        if let Some(previous) = self.results.get(&result.pass_id) {
            if previous.status == EnterpriseReviewPassStatus::Completed {
                return Err(EnterpriseReviewPlanError::DuplicatePass);
            }
            if result.attempt <= previous.attempt {
                return Err(EnterpriseReviewPlanError::InvalidPass("attempt order"));
            }
        }
        if result.schema != ENTERPRISE_REVIEW_PLAN_SCHEMA
            || result.attempt == 0
            || result.attempt > MAX_ENTERPRISE_REVIEW_PASS_ATTEMPTS
            || result.requests == 0
            || result.requests > pass.request_budget
            || result.tokens > pass.token_budget
            || result.duration_ms > pass.duration_budget_ms
            || result.findings.len() > MAX_ENTERPRISE_REVIEW_FINDINGS_PER_PASS
        {
            return Err(EnterpriseReviewPlanError::InvalidPass("result bounds"));
        }
        validate_findings(&result.findings)?;
        if result.status != EnterpriseReviewPassStatus::Completed {
            result.findings.clear();
        }
        let requests = self.requests_used.saturating_add(result.requests);
        let tokens = self.tokens_used.saturating_add(result.tokens);
        let duration = self.duration_used_ms.saturating_add(result.duration_ms);
        if requests > self.plan.max_requests {
            return Err(EnterpriseReviewPlanError::BudgetExceeded("requests"));
        }
        if tokens > self.plan.max_tokens {
            return Err(EnterpriseReviewPlanError::BudgetExceeded("tokens"));
        }
        if duration > self.plan.max_duration_ms {
            return Err(EnterpriseReviewPlanError::BudgetExceeded("duration_ms"));
        }
        self.requests_used = requests;
        self.tokens_used = tokens;
        self.duration_used_ms = duration;
        self.history.push(result.clone());
        self.results.insert(result.pass_id.clone(), result);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn checkpoint(&self) -> EnterpriseReviewCheckpoint {
        EnterpriseReviewCheckpoint {
            schema: ENTERPRISE_REVIEW_CHECKPOINT_SCHEMA.to_owned(),
            review_id: self.plan.review_id.clone(),
            plan_digest: self.plan.plan_digest.clone(),
            revision: self.revision,
            results: self.history.clone(),
            requests_used: self.requests_used,
            tokens_used: self.tokens_used,
            duration_used_ms: self.duration_used_ms,
        }
    }

    pub fn resume(
        plan: EnterpriseReviewPlan,
        checkpoint: EnterpriseReviewCheckpoint,
    ) -> Result<Self, EnterpriseReviewPlanError> {
        let mut run = Self::start(plan)?;
        if checkpoint.schema != ENTERPRISE_REVIEW_CHECKPOINT_SCHEMA
            || checkpoint.review_id != run.plan.review_id
            || checkpoint.plan_digest != run.plan.plan_digest
            || checkpoint.revision == 0
        {
            return Err(EnterpriseReviewPlanError::PlanMismatch);
        }
        for result in checkpoint.results {
            run.record_pass(result)?;
        }
        if run.revision != checkpoint.revision
            || run.requests_used != checkpoint.requests_used
            || run.tokens_used != checkpoint.tokens_used
            || run.duration_used_ms != checkpoint.duration_used_ms
        {
            return Err(EnterpriseReviewPlanError::CheckpointInvalid("counters"));
        }
        Ok(run)
    }

    pub fn finalize(&self) -> Result<EnterpriseReviewOutcome, EnterpriseReviewPlanError> {
        let completed_passes = self
            .results
            .values()
            .filter(|result| result.status == EnterpriseReviewPassStatus::Completed)
            .count();
        if completed_passes != self.plan.passes.len() {
            return Err(EnterpriseReviewPlanError::NotComplete);
        }
        let mut findings = BTreeMap::<String, EnterpriseReviewFindingRef>::new();
        for result in self.results.values() {
            for finding in &result.findings {
                findings
                    .entry(finding.finding_fingerprint.clone())
                    .or_insert_with(|| finding.clone());
            }
        }
        let confirmed_findings = findings
            .values()
            .filter(|finding| finding.confirmed)
            .count();
        Ok(EnterpriseReviewOutcome {
            schema: ENTERPRISE_REVIEW_OUTCOME_SCHEMA.to_owned(),
            review_id: self.plan.review_id.clone(),
            plan_digest: self.plan.plan_digest.clone(),
            completed_passes,
            unique_findings: findings.len(),
            confirmed_findings,
            requests_used: self.requests_used,
            tokens_used: self.tokens_used,
            duration_used_ms: self.duration_used_ms,
            read_only: true,
            network_egress: false,
            workspace_mutated: false,
            secret_free: true,
            // A plan/result is execution evidence, not the independent live
            // paired quality campaign required by Roadmap Stage 12.
            quality_claim_eligible: false,
        })
    }
}

impl EnterpriseReviewWorkPlan {
    /// Validate a work projection after transport or durable-store recovery.
    /// This keeps a broker from materializing a stale, duplicated, or
    /// broadened pass even when the source `EnterpriseReviewPlan` is no longer
    /// in memory.
    pub fn validate(&self) -> Result<(), EnterpriseReviewPlanError> {
        if self.schema != ENTERPRISE_REVIEW_WORK_PLAN_SCHEMA
            || self.work_items.len() != MAX_ENTERPRISE_REVIEW_PASSES
            || self.work_plan_digest != work_plan_digest(self)
            || !valid_opaque_id(&self.review_id)
            || !valid_fingerprint(&self.plan_digest)
            || !valid_fingerprint(&self.repository_fingerprint)
            || !valid_fingerprint(&self.scope_fingerprint)
        {
            return Err(EnterpriseReviewPlanError::PlanMismatch);
        }
        self.admission
            .validate()
            .map_err(|_| EnterpriseReviewPlanError::InvalidField("admission"))?;
        let mut keys = BTreeSet::new();
        let mut pass_ids = BTreeSet::new();
        for item in &self.work_items {
            if item.schema != ENTERPRISE_REVIEW_WORK_PLAN_SCHEMA
                || item.review_id != self.review_id
                || item.pass_id != format!("{}:{}", self.review_id, item.kind.id())
                || !valid_fingerprint(&item.work_key)
                || !valid_fingerprint(&item.objective_digest)
                || item.template.kind != format!("enterprise_review:{}", item.kind.id())
            {
                return Err(EnterpriseReviewPlanError::InvalidField("work_item"));
            }
            item.template
                .validate()
                .map_err(|_| EnterpriseReviewPlanError::InvalidField("work_template"))?;
            if !keys.insert(&item.work_key) || !pass_ids.insert(&item.pass_id) {
                return Err(EnterpriseReviewPlanError::DuplicatePass);
            }
        }
        Ok(())
    }
}

fn validate_findings(
    findings: &[EnterpriseReviewFindingRef],
) -> Result<(), EnterpriseReviewPlanError> {
    let mut ids = BTreeSet::new();
    for finding in findings {
        if !ids.insert(finding.finding_fingerprint.clone())
            || !valid_fingerprint(&finding.finding_fingerprint)
            || finding.line_start == 0
            || finding.line_end < finding.line_start
            || finding.confidence_bps > 10_000
            || finding.category.is_empty()
            || finding.category.len() > 96
            || !safe_location(&finding.location)
        {
            return Err(EnterpriseReviewPlanError::InvalidField("finding"));
        }
    }
    Ok(())
}

fn safe_location(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ENTERPRISE_REVIEW_LOCATION_BYTES
        && !value.starts_with('/')
        && !value.starts_with('~')
        && !value.contains('\\')
        && !value.split('/').any(|part| part == "..")
        && !value.chars().any(char::is_control)
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn plan_digest(plan: &EnterpriseReviewPlan) -> String {
    let mut unsigned = plan.clone();
    unsigned.plan_digest.clear();
    digest(&serde_json::to_string(&unsigned).expect("plan serialization is infallible"))
}

fn work_plan_digest(work_plan: &EnterpriseReviewWorkPlan) -> String {
    let mut unsigned = work_plan.clone();
    unsigned.work_plan_digest.clear();
    digest(&serde_json::to_string(&unsigned).expect("work plan serialization is infallible"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn lease(now: DateTime<Utc>) -> EnterpriseReviewLease {
        let mut lease = EnterpriseReviewLease {
            schema: super::super::enterprise_review::ENTERPRISE_REVIEW_LEASE_SCHEMA.into(),
            lease_id: "lease-plan".into(),
            credential_id: "credential-plan".into(),
            route_id: "company-gateway".into(),
            endpoint_fingerprint: "a".repeat(64),
            model_id: "modest-review-v1".into(),
            model_tier: super::super::enterprise_review::EnterpriseModelTier::Modest,
            issued_at: now - Duration::minutes(1),
            expires_at: now + Duration::hours(2),
            route_binding_digest: String::new(),
            read_only: true,
            allow_network: false,
            allow_workspace_writes: false,
            allow_publication: false,
            max_requests: super::super::enterprise_review::MAX_ENTERPRISE_REVIEW_REQUESTS,
            max_tokens: super::super::enterprise_review::MAX_ENTERPRISE_REVIEW_TOKENS,
            max_duration_ms: super::super::enterprise_review::MAX_ENTERPRISE_REVIEW_DURATION_MS,
            attestation: super::super::enterprise_review::EnterpriseGatewayAttestation {
                schema: super::super::enterprise_review::ENTERPRISE_REVIEW_ATTESTATION_SCHEMA
                    .into(),
                route_id: "company-gateway".into(),
                endpoint_fingerprint: "a".repeat(64),
                model_id: "modest-review-v1".into(),
                model_tier: super::super::enterprise_review::EnterpriseModelTier::Modest,
                deployment_revision: "deployment-plan".into(),
                issued_at: now - Duration::minutes(1),
                expires_at: now + Duration::hours(2),
                no_premium_fallback: true,
                egress_firewall_attested: true,
                signing_key_id: None,
                signature: None,
            },
        };
        lease.route_binding_digest =
            super::super::enterprise_review::expected_route_binding_digest(&lease);
        lease
    }

    fn plan() -> EnterpriseReviewPlan {
        let now = Utc::now();
        build_enterprise_review_plan(
            &lease(now),
            &EnterpriseReviewPolicy::default(),
            now,
            "review-plan",
            "b".repeat(64),
            "c".repeat(64),
        )
        .unwrap()
    }

    fn result(
        pass: &EnterpriseReviewPass,
        attempt: u32,
        status: EnterpriseReviewPassStatus,
    ) -> EnterpriseReviewPassResult {
        EnterpriseReviewPassResult {
            schema: ENTERPRISE_REVIEW_PLAN_SCHEMA.into(),
            pass_id: pass.pass_id.clone(),
            attempt,
            status,
            requests: 1,
            tokens: 10,
            duration_ms: 10,
            findings: vec![EnterpriseReviewFindingRef {
                finding_fingerprint: "d".repeat(64),
                location: "src/lib.rs".into(),
                line_start: 10,
                line_end: 12,
                category: "security".into(),
                confidence_bps: 9_000,
                confirmed: true,
            }],
        }
    }

    #[test]
    fn plan_is_deterministic_and_has_all_specialized_passes() {
        let first = plan();
        let second = plan();
        assert_eq!(first.plan_digest, second.plan_digest);
        assert_eq!(first.passes.len(), MAX_ENTERPRISE_REVIEW_PASSES);
        assert_eq!(
            first
                .passes
                .iter()
                .map(|pass| pass.kind)
                .collect::<Vec<_>>(),
            EnterpriseReviewPassKind::ALL
        );
        assert!(first
            .passes
            .iter()
            .all(|pass| valid_fingerprint(&pass.objective_digest)));
        for pass in &first.passes {
            let template = pass.work_template();
            template.validate().unwrap();
            assert!(template.objective.contains(&pass.objective_digest));
            assert!(!template.objective.contains("https://"));
        }
    }

    #[test]
    fn work_plan_is_stable_parallel_and_secret_free() {
        let first = plan().work_plan().unwrap();
        let second = plan().work_plan().unwrap();
        first.validate().unwrap();
        second.validate().unwrap();
        first.admission.validate().unwrap();
        assert_eq!(first.work_plan_digest, second.work_plan_digest);
        assert_eq!(first.work_items.len(), MAX_ENTERPRISE_REVIEW_PASSES);
        assert!(first
            .work_items
            .windows(2)
            .all(|items| items[0].work_key != items[1].work_key));
        assert!(first.work_items.iter().all(|item| {
            item.schema == ENTERPRISE_REVIEW_WORK_PLAN_SCHEMA
                && item.review_id == first.review_id
                && item.template.validate().is_ok()
                && item.template.objective.contains(&item.objective_digest)
        }));
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(encoded.contains("company-gateway"));
        assert!(!encoded.contains("credential-plan"));
        assert!(!encoded.contains("https://"));

        let mut unknown_nested = serde_json::to_value(&first).unwrap();
        unknown_nested["work_items"][0]["template"]["unexpectedPolicyField"] =
            serde_json::json!(true);
        assert!(serde_json::from_value::<EnterpriseReviewWorkPlan>(unknown_nested).is_err());

        let mut invalid_work_plan = first.clone();
        invalid_work_plan.work_items[0].work_key = "0".repeat(64);
        assert_eq!(
            invalid_work_plan.validate(),
            Err(EnterpriseReviewPlanError::PlanMismatch)
        );

        let mut invalid_admission = first.clone();
        invalid_admission.admission.read_only = false;
        invalid_admission.work_plan_digest = work_plan_digest(&invalid_admission);
        assert_eq!(
            invalid_admission.validate(),
            Err(EnterpriseReviewPlanError::InvalidField("admission"))
        );

        let mut tampered = plan();
        tampered.plan_digest = "f".repeat(64);
        assert_eq!(
            tampered.work_plan(),
            Err(EnterpriseReviewPlanError::PlanMismatch)
        );
        assert_eq!(
            enterprise_review_work_request_id(&first.plan_digest, &first.work_items[0].work_key),
            format!(
                "enterprise-review:{}:{}",
                first.plan_digest, first.work_items[0].work_key
            )
        );
    }

    #[test]
    fn checkpoint_resume_deduplicates_findings_and_never_claims_quality() {
        let plan = plan();
        let mut run = EnterpriseReviewRun::start(plan.clone()).unwrap();
        for pass in plan.passes.iter().take(3) {
            run.record_pass(result(pass, 1, EnterpriseReviewPassStatus::Completed))
                .unwrap();
        }
        let checkpoint = run.checkpoint();
        let mut resumed = EnterpriseReviewRun::resume(plan.clone(), checkpoint).unwrap();
        for pass in plan.passes.iter().skip(3) {
            resumed
                .record_pass(result(pass, 1, EnterpriseReviewPassStatus::Completed))
                .unwrap();
        }
        let outcome = resumed.finalize().unwrap();
        assert_eq!(outcome.completed_passes, MAX_ENTERPRISE_REVIEW_PASSES);
        assert_eq!(outcome.unique_findings, 1);
        assert_eq!(outcome.confirmed_findings, 1);
        assert!(!outcome.quality_claim_eligible);
        assert!(outcome.read_only && !outcome.network_egress && !outcome.workspace_mutated);
    }

    #[test]
    fn invalid_locations_and_plan_or_budget_drift_fail_closed() {
        let plan = plan();
        let mut run = EnterpriseReviewRun::start(plan.clone()).unwrap();
        let mut unsafe_result = result(&plan.passes[0], 1, EnterpriseReviewPassStatus::Completed);
        unsafe_result.findings[0].location = "/private/secret.rs".into();
        assert!(matches!(
            run.record_pass(unsafe_result),
            Err(EnterpriseReviewPlanError::InvalidField("finding"))
        ));

        let mut drifted = plan.clone();
        drifted.scope_fingerprint = "e".repeat(64);
        assert!(matches!(
            EnterpriseReviewRun::start(drifted),
            Err(EnterpriseReviewPlanError::PlanMismatch)
        ));

        let mut interrupted = result(&plan.passes[0], 1, EnterpriseReviewPassStatus::Interrupted);
        interrupted.requests = plan.passes[0].request_budget + 1;
        assert!(matches!(
            run.record_pass(interrupted),
            Err(EnterpriseReviewPlanError::InvalidPass("result bounds"))
        ));
    }

    #[test]
    fn interrupted_pass_can_retry_once_with_auditable_attempt_history() {
        let plan = plan();
        let mut run = EnterpriseReviewRun::start(plan.clone()).unwrap();
        run.record_pass(result(
            &plan.passes[0],
            1,
            EnterpriseReviewPassStatus::Interrupted,
        ))
        .unwrap();
        run.record_pass(result(
            &plan.passes[0],
            2,
            EnterpriseReviewPassStatus::Completed,
        ))
        .unwrap();
        let checkpoint = run.checkpoint();
        assert_eq!(checkpoint.results.len(), 2);
        assert_eq!(checkpoint.results[0].attempt, 1);
        assert_eq!(checkpoint.results[1].attempt, 2);
        assert!(matches!(
            run.record_pass(result(
                &plan.passes[0],
                3,
                EnterpriseReviewPassStatus::Completed,
            )),
            Err(EnterpriseReviewPlanError::DuplicatePass)
        ));
    }
}
