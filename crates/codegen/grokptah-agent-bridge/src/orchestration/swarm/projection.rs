//! Secret-free typed projections for manager, MCP, and desktop surfaces.
//!
//! Every DTO here is built by naming the fields it carries, never by
//! serializing a durable record wholesale. That is the property that makes the
//! absence of a credential checkable: `WorkerSpec::credential_ref` is a
//! keychain reference and simply has no field to land in, and all free-form
//! text passes through the caller's redactor and a byte bound that cuts on a
//! character boundary.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::authority::{AttemptState, RetryClass, SendCertainty};
use super::scheduler::AdmissionBlock;
use super::state::{
    truncate_text, GraphLifecycle, LeaseState, ReviewVerdict, WorkGraphRecord, WorkState,
};

/// Maximum bytes of free-form text any projected field may carry.
pub const MAX_PROJECTED_TEXT_BYTES: usize = 500;

/// How a caller redacts free-form text before it is projected.
///
/// The bridge passes the event bus's redactor, so control-plane secrets
/// registered there are stripped from graph projections by the same rule that
/// covers the durable journal.
pub trait Redactor {
    fn redact(&self, text: &str, max_bytes: usize) -> String;
}

/// A redactor that only bounds text. Used where no secret registry exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct BoundOnlyRedactor;

impl Redactor for BoundOnlyRedactor {
    fn redact(&self, text: &str, max_bytes: usize) -> String {
        truncate_text(text, max_bytes)
    }
}

impl<F> Redactor for F
where
    F: Fn(&str, usize) -> String,
{
    fn redact(&self, text: &str, max_bytes: usize) -> String {
        self(text, max_bytes)
    }
}

fn project_text(redactor: &dyn Redactor, text: &str) -> String {
    truncate_text(
        &redactor.redact(text, MAX_PROJECTED_TEXT_BYTES),
        MAX_PROJECTED_TEXT_BYTES,
    )
}

/// One row of graph progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkProgressRow {
    pub work_id: String,
    pub worker_id: String,
    pub role: String,
    pub state: WorkState,
    pub attempts: u32,
    pub priority: i32,
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Secret-free provider attribution for this item's latest attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_key: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// One row of redacted evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRow {
    pub work_id: String,
    pub label: String,
    pub detail: String,
}

/// One lease, as an operator sees it. Carries no authority material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRow {
    pub lease_id: String,
    pub work_id: String,
    pub worker_id: String,
    pub attempt: u32,
    pub epoch: u64,
    pub state: LeaseState,
    /// Whether a Computer Use grant is bound, never which one or its material.
    pub computer_use_bound: bool,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncertain_reason: Option<String>,
}

/// Secret-free per-provider accounting for a mixed-provider graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttributionRow {
    /// `provider/profile/model@effort`. Never a credential or an endpoint URL.
    pub attribution_key: String,
    pub attempts: u32,
    pub finished: u32,
    pub uncertain: u32,
    pub tokens: u64,
}

/// Whole-graph status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStatusProjection {
    pub graph_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub revision: u64,
    pub epoch: u64,
    pub lifecycle: GraphLifecycle,
    pub work_total: usize,
    pub in_flight: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub blocked: usize,
    pub discarded: usize,
    pub uncertain: usize,
    /// True while any child's fate is unknown. An operator surface should treat
    /// this as "do not conclude anything yet".
    pub needs_operator_attention: bool,
    pub attempts_used: u32,
    pub max_total_attempts: u32,
    pub tokens_used: u64,
    pub max_total_tokens: u64,
    pub admissions_refused: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission_block: Option<AdmissionBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub deadline_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The full operator projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphProjection {
    pub status: GraphStatusProjection,
    pub work: Vec<WorkProgressRow>,
    pub leases: Vec<LeaseRow>,
    pub attribution: Vec<ProviderAttributionRow>,
}

/// Project whole-graph status.
pub fn project_status(
    record: &WorkGraphRecord,
    admission_block: Option<AdmissionBlock>,
    redactor: &dyn Redactor,
) -> GraphStatusProjection {
    let count = |predicate: fn(WorkState) -> bool| {
        record
            .work
            .iter()
            .filter(|item| predicate(item.state))
            .count()
    };
    GraphStatusProjection {
        graph_id: record.graph_id.to_string(),
        session_id: record.session_id.to_string(),
        agent_id: project_text(redactor, &record.agent_id),
        revision: record.revision,
        epoch: record.epoch,
        lifecycle: record.lifecycle,
        work_total: record.work.len(),
        in_flight: record.in_flight(),
        succeeded: count(|state| state == WorkState::Succeeded),
        failed: count(|state| matches!(state, WorkState::Failed | WorkState::TimedOut)),
        blocked: count(|state| state == WorkState::Blocked),
        discarded: count(|state| state == WorkState::Discarded),
        uncertain: count(|state| state == WorkState::DispatchUncertain),
        needs_operator_attention: record.has_uncertainty(),
        attempts_used: record.budget.attempts_used,
        max_total_attempts: record.spec.budget.max_total_attempts,
        tokens_used: record.budget.tokens_used,
        max_total_tokens: record.spec.budget.max_total_tokens,
        admissions_refused: record.budget.admissions_refused,
        admission_block: admission_block.filter(|block| *block != AdmissionBlock::None),
        stop_reason: record
            .stop_reason
            .as_deref()
            .map(|text| project_text(redactor, text)),
        deadline_at: record.deadline_at,
        updated_at: record.updated_at,
    }
}

/// Project per-item progress in deterministic work-id order.
pub fn project_work(record: &WorkGraphRecord, redactor: &dyn Redactor) -> Vec<WorkProgressRow> {
    let mut rows: Vec<WorkProgressRow> = record
        .work
        .iter()
        .filter_map(|item| {
            let spec = record.spec.work_item(&item.work_id)?;
            let role = record.spec.role_of(&item.work_id)?;
            let attribution_key = record
                .attempts
                .iter()
                .filter(|attempt| attempt.work_id == item.work_id)
                .max_by_key(|attempt| attempt.ordinal)
                .map(|attempt| attempt.attribution_key.clone());
            Some(WorkProgressRow {
                work_id: item.work_id.to_string(),
                worker_id: spec.worker_id.to_string(),
                role: format!("{role:?}").to_lowercase(),
                state: item.state,
                attempts: item.attempts,
                priority: spec.priority,
                depends_on: spec.depends_on.iter().map(ToString::to_string).collect(),
                verdict: item.verdict,
                summary: item
                    .summary
                    .as_deref()
                    .map(|text| project_text(redactor, text)),
                last_error: item
                    .last_error
                    .as_deref()
                    .map(|text| project_text(redactor, text)),
                attribution_key,
                updated_at: item.updated_at,
            })
        })
        .collect();
    rows.sort_by(|left, right| left.work_id.cmp(&right.work_id));
    rows
}

/// Project bounded, redacted evidence in deterministic order.
pub fn project_evidence(record: &WorkGraphRecord, redactor: &dyn Redactor) -> Vec<EvidenceRow> {
    let mut rows = Vec::new();
    for item in &record.work {
        for entry in &item.evidence {
            rows.push(EvidenceRow {
                work_id: item.work_id.to_string(),
                label: project_text(redactor, &entry.label),
                detail: project_text(redactor, &entry.detail),
            });
        }
    }
    rows.sort_by(|left, right| {
        left.work_id
            .cmp(&right.work_id)
            .then_with(|| left.label.cmp(&right.label))
    });
    rows
}

/// Project leases without any authority material.
pub fn project_leases(record: &WorkGraphRecord, redactor: &dyn Redactor) -> Vec<LeaseRow> {
    let mut rows: Vec<LeaseRow> = record
        .leases
        .iter()
        .map(|lease| LeaseRow {
            lease_id: lease.lease_id.to_string(),
            work_id: lease.work_id.to_string(),
            worker_id: lease.worker_id.to_string(),
            attempt: lease.attempt,
            epoch: lease.epoch,
            state: lease.state,
            computer_use_bound: lease.grant.is_some(),
            issued_at: lease.issued_at,
            expires_at: lease.expires_at,
            uncertain_reason: lease
                .uncertain_reason
                .as_deref()
                .map(|text| project_text(redactor, text)),
        })
        .collect();
    rows.sort_by(|left, right| left.lease_id.cmp(&right.lease_id));
    rows
}

/// Project mixed-provider accounting.
pub fn project_attribution(record: &WorkGraphRecord) -> Vec<ProviderAttributionRow> {
    let mut by_key: BTreeMap<&str, ProviderAttributionRow> = BTreeMap::new();
    for attempt in &record.attempts {
        let row = by_key
            .entry(attempt.attribution_key.as_str())
            .or_insert_with(|| ProviderAttributionRow {
                attribution_key: attempt.attribution_key.clone(),
                attempts: 0,
                finished: 0,
                uncertain: 0,
                tokens: 0,
            });
        row.attempts = row.attempts.saturating_add(1);
        if attempt.state == AttemptState::Finished {
            row.finished = row.finished.saturating_add(1);
        }
        if attempt.send_certainty == Some(SendCertainty::UncertainAccept)
            || attempt.retry_class == Some(RetryClass::ExplicitNewAttemptOnly)
                && attempt.send_certainty != Some(SendCertainty::KnownAccepted)
        {
            row.uncertain = row.uncertain.saturating_add(1);
        }
        if let Some(usage) = &attempt.usage {
            let total = usage
                .total_tokens
                .max(usage.prompt_tokens.saturating_add(usage.completion_tokens));
            row.tokens = row.tokens.saturating_add(total);
        }
    }
    by_key.into_values().collect()
}

/// Project everything an operator surface needs.
pub fn project_graph(
    record: &WorkGraphRecord,
    admission_block: Option<AdmissionBlock>,
    redactor: &dyn Redactor,
) -> GraphProjection {
    GraphProjection {
        status: project_status(record, admission_block, redactor),
        work: project_work(record, redactor),
        leases: project_leases(record, redactor),
        attribution: project_attribution(record),
    }
}

/// A deliberately narrow desktop/UI DTO.
///
/// The browser and UI hold no authority: this carries counts, states, and
/// bounded text, and there is no field through which a token, a credential
/// reference, an endpoint, or a lease secret could travel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGraphDto {
    pub graph_id: String,
    pub lifecycle: GraphLifecycle,
    pub work_total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub in_flight: usize,
    pub uncertain: usize,
    pub needs_operator_attention: bool,
    pub attempts_used: u32,
    pub max_total_attempts: u32,
    pub updated_at: DateTime<Utc>,
}

/// Project the desktop DTO from a full status projection.
pub fn project_desktop(status: &GraphStatusProjection) -> DesktopGraphDto {
    DesktopGraphDto {
        graph_id: status.graph_id.clone(),
        lifecycle: status.lifecycle,
        work_total: status.work_total,
        succeeded: status.succeeded,
        failed: status.failed,
        in_flight: status.in_flight,
        uncertain: status.uncertain,
        needs_operator_attention: status.needs_operator_attention,
        attempts_used: status.attempts_used,
        max_total_attempts: status.max_total_attempts,
        updated_at: status.updated_at,
    }
}
