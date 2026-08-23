//! Allowlisted public Run projection.
//!
//! Durable [`super::types::RunRecord`] values retain the frozen
//! [`super::types::ProviderRouteSnapshot`], including endpoint and credential
//! identity. Public list/get/progress surfaces must serialize this type
//! instead of the persistence record. Fields that are not on this type cannot
//! appear on the wire.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::completion::{
    CompletionClaims, CompletionEvidence, CompletionObservations, CompletionUsage,
};
use crate::gateway_config::{
    CapabilitySource, ProviderDeadlineClass, ProviderDialect, ProviderKind,
};
use crate::types::EffortLevel;

use super::provider_attempt::{
    ProviderAttemptRecord, ProviderAttemptState, ProviderRetryClass, ProviderSendCertainty,
};
use super::quota::{QuotaClass, QuotaReservation, QuotaReservationState};
use super::store::OrchStore;
use super::store::MAX_PUBLIC_RUN_LIST;
use super::types::{
    OrchError, OrchErrorCode, PromotionState, ProviderRouteSnapshot, RunAggregates, RunApproval,
    RunBounds, RunExecution, RunExecutionMode, RunProgress, RunPurpose, RunRecord, RunState,
    RunStopCause,
};

pub const PUBLIC_ERROR_PRIVILEGED_DIAGNOSTICS: &str = "privileged_diagnostics";
pub const PUBLIC_RUN_LIST_PAGE_LIMIT: usize = MAX_PUBLIC_RUN_LIST;

const FORBIDDEN_PUBLIC_RUN_KEYS: &[&str] = &[
    "apiKey",
    "api_key",
    "authorization",
    "baseUrl",
    "base_url",
    "bearer",
    "credentialFingerprint",
    "credential_fingerprint",
    "credentialRef",
    "credential_ref",
    "endpointFingerprint",
    "endpoint_fingerprint",
    "password",
    "providerRoute",
    "provider_route",
    "qualificationRecordId",
    "qualification_record_id",
    "quotaReservationId",
    "quota_reservation_id",
    "secret",
    "selectionKey",
    "selection_key",
    "token",
];

/// Exact public `providerExecution.route` key allowlist.
pub const PUBLIC_PROVIDER_ROUTE_KEYS: &[&str] = &[
    "capabilitySource",
    "deadlineClass",
    "dialect",
    "effort",
    "kind",
    "modelId",
    "providerId",
    "qualificationSchema",
    "snapshotHash",
    "wireModelId",
];

/// Version stamp for promote/discard idempotency receipts.
pub const PUBLIC_RUN_RECEIPT_SCHEMA: &str = "grokptah.public-run-receipt.v1";

/// Public usage counters. Nested unknown keys must fail trusted decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicCompletionUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
}

impl From<&CompletionUsage> for PublicCompletionUsage {
    fn from(usage: &CompletionUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            requests: usage.requests,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicCompletionClaims {
    pub present: bool,
    pub mentions_changes: bool,
    pub mentions_tests: bool,
    pub mentions_verification: bool,
}

impl From<&CompletionClaims> for PublicCompletionClaims {
    fn from(claims: &CompletionClaims) -> Self {
        Self {
            present: claims.present,
            mentions_changes: claims.mentions_changes,
            mentions_tests: claims.mentions_tests,
            mentions_verification: claims.mentions_verification,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicCompletionObservations {
    pub changed_files: u32,
    pub tests_observed: u32,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_incomplete: u32,
    pub permissions_requested: u32,
    pub permissions_granted: u32,
    pub permissions_denied: u32,
    pub permissions_unresolved: u32,
}

impl From<&CompletionObservations> for PublicCompletionObservations {
    fn from(observations: &CompletionObservations) -> Self {
        Self {
            changed_files: observations.changed_files,
            tests_observed: observations.tests_observed,
            tests_passed: observations.tests_passed,
            tests_failed: observations.tests_failed,
            tests_incomplete: observations.tests_incomplete,
            permissions_requested: observations.permissions_requested,
            permissions_granted: observations.permissions_granted,
            permissions_denied: observations.permissions_denied,
            permissions_unresolved: observations.permissions_unresolved,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicCompletionEvidence {
    pub status: String,
    pub stop_reason: String,
    pub interrupted: bool,
    pub claims: PublicCompletionClaims,
    pub observations: PublicCompletionObservations,
    pub usage: PublicCompletionUsage,
}

impl From<&CompletionEvidence> for PublicCompletionEvidence {
    fn from(evidence: &CompletionEvidence) -> Self {
        Self {
            status: evidence.status.clone(),
            stop_reason: evidence.stop_reason.clone(),
            interrupted: evidence.interrupted,
            claims: PublicCompletionClaims::from(&evidence.claims),
            observations: PublicCompletionObservations::from(&evidence.observations),
            usage: PublicCompletionUsage::from(&evidence.usage),
        }
    }
}

/// Secret-free route identity that operators and coordinators may observe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicProviderRouteSummary {
    pub provider_id: String,
    pub kind: ProviderKind,
    pub dialect: ProviderDialect,
    pub model_id: String,
    pub wire_model_id: String,
    pub capability_source: CapabilitySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualification_schema: Option<String>,
    pub deadline_class: ProviderDeadlineClass,
    pub effort: EffortLevel,
    pub snapshot_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicQuotaLimits {
    pub window_ms: u64,
    pub max_in_flight_reservations: u32,
    pub max_tokens_per_window: u64,
    pub max_requests_per_window: u64,
}

/// Quota row joined into the public provider-execution summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicProviderQuota {
    pub reservation_id: String,
    pub pool_id: String,
    pub class: QuotaClass,
    pub state: QuotaReservationState,
    pub tokens_reserved: u64,
    pub tokens_consumed: u64,
    pub requests_reserved: u64,
    pub requests_consumed: u64,
    pub window_started_at: DateTime<Utc>,
    pub limits: PublicQuotaLimits,
    pub updated_at: DateTime<Utc>,
}

/// One durable provider attempt in the public summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicProviderAttempt {
    pub attempt_id: String,
    pub ordinal: u64,
    pub state: ProviderAttemptState,
    pub send_certainty: Option<ProviderSendCertainty>,
    pub retry_class: Option<ProviderRetryClass>,
    pub http_status: Option<u16>,
    pub usage: Option<PublicCompletionUsage>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Bounded provider-execution overlay shared by list, get, and progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicProviderExecution {
    pub route: PublicProviderRouteSummary,
    pub quota: Option<PublicProviderQuota>,
    pub attempts: Vec<PublicProviderAttempt>,
    pub attempt_count: usize,
    pub attempts_truncated: bool,
    pub usage_complete: bool,
    pub pending_requests: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunBounds {
    pub max_prompt_bytes: usize,
    pub max_rounds: u32,
    pub max_duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicChangeRecord {
    pub path: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicTestObservation {
    pub call_id: String,
    pub command: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub cancelled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunAggregates {
    pub changes: Vec<PublicChangeRecord>,
    pub tests: Vec<PublicTestObservation>,
    #[serde(default)]
    pub permissions_requested: u32,
    #[serde(default)]
    pub permissions_granted: u32,
    #[serde(default)]
    pub permissions_denied: u32,
    #[serde(default)]
    pub usage: PublicCompletionUsage,
    #[serde(default)]
    pub usage_complete: bool,
    #[serde(default)]
    pub usage_pending_requests: u32,
    #[serde(default)]
    pub verification: Option<PublicCompletionEvidence>,
}

impl Default for PublicRunAggregates {
    fn default() -> Self {
        Self {
            changes: Vec::new(),
            tests: Vec::new(),
            permissions_requested: 0,
            permissions_granted: 0,
            permissions_denied: 0,
            usage: PublicCompletionUsage::default(),
            usage_complete: true,
            usage_pending_requests: 0,
            verification: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunProgressDetail {
    pub round: u32,
    pub max_rounds: u32,
    pub last_tool: Option<String>,
    pub detail: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunExecution {
    pub mode: RunExecutionMode,
    pub source_workspace: String,
    pub execution_workspace: String,
    pub base_revision: String,
    pub source_fingerprint: String,
    #[serde(default)]
    pub final_fingerprint: Option<String>,
    #[serde(default)]
    pub promotion_state: PromotionState,
    #[serde(default)]
    pub promoted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunApproval {
    pub approval_id: String,
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub source_fingerprint: String,
    pub final_fingerprint: String,
    pub changed_files: Vec<PublicChangeRecord>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Allowlisted public Run. This is the only Run shape MCP, hosted service,
/// local Tauri, and remote-desktop decoding may serialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRun {
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub request_id: String,
    pub client_id: Option<String>,
    pub state: RunState,
    #[serde(default)]
    pub purpose: RunPurpose,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub retry_of: Option<String>,
    #[serde(default)]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub agent_spec_revision: Option<u64>,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    #[serde(default)]
    pub continuation_context_id: Option<String>,
    #[serde(default)]
    pub continuation_context_hash: Option<String>,
    #[serde(default)]
    pub continuation_fidelity: Option<String>,
    #[serde(default)]
    pub queue_position: Option<usize>,
    pub bounds: PublicRunBounds,
    pub prompt_preview: String,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminal_result: Option<String>,
    pub final_response: Option<String>,
    pub error_code: Option<String>,
    #[serde(default)]
    pub stop_cause: Option<RunStopCause>,
    #[serde(default)]
    pub aggregates: PublicRunAggregates,
    #[serde(default)]
    pub progress: Option<PublicRunProgressDetail>,
    #[serde(default)]
    pub execution: Option<PublicRunExecution>,
    #[serde(default)]
    pub approval: Option<PublicRunApproval>,
    #[serde(default)]
    pub provider_execution: Option<PublicProviderExecution>,
}

/// Allowlisted progress view. Built from the same provider-execution helper
/// as [`PublicRun`]; it never serializes a [`RunRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunProgress {
    pub run_id: String,
    pub session_id: Uuid,
    pub state: RunState,
    pub queue_position: Option<usize>,
    pub busy: bool,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
    pub prompt_preview: String,
    pub progress: Option<PublicRunProgressDetail>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminal_result: Option<String>,
    pub stop_cause: Option<RunStopCause>,
    pub bounds: PublicRunBounds,
    pub error_code: Option<String>,
    pub provider_execution: Option<PublicProviderExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunPage {
    pub runs: Vec<PublicRun>,
    pub total_count: usize,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublicRunReceiptV1 {
    schema: String,
    run: PublicRun,
}

/// Project one persisted Run into the public allowlist.
pub fn project_public_run(store: &OrchStore, run: &RunRecord) -> Result<PublicRun, OrchError> {
    let provider_execution = project_provider_execution(store, run)?;
    let mut projected = PublicRun {
        run_id: run.run_id.clone(),
        session_id: run.session_id,
        workspace: run.workspace.clone(),
        request_id: run.request_id.clone(),
        client_id: run.client_id.clone(),
        state: run.state,
        purpose: run.purpose,
        agent_id: run.agent_id.clone(),
        retry_of: run.retry_of.clone(),
        parent_run_id: run.parent_run_id.clone(),
        agent_spec_revision: run.agent_spec_revision,
        checkpoint_id: run.checkpoint_id.clone(),
        continuation_context_id: run.continuation_context_id.clone(),
        continuation_context_hash: run.continuation_context_hash.clone(),
        continuation_fidelity: run.continuation_fidelity.clone(),
        queue_position: run.queue_position,
        bounds: project_bounds(&run.bounds),
        prompt_preview: run.prompt_preview.clone(),
        start_seq: run.start_seq,
        end_seq: run.end_seq,
        created_at: run.created_at,
        updated_at: run.updated_at,
        terminal_result: run.terminal_result.clone(),
        final_response: run.final_response.clone(),
        error_code: run.error_code.clone(),
        stop_cause: run.stop_cause,
        aggregates: project_aggregates(&run.aggregates),
        progress: run.progress.as_ref().map(project_progress_detail),
        execution: run.execution.as_ref().map(project_execution),
        approval: run.approval.as_ref().map(project_approval),
        provider_execution,
    };
    scrub_public_run(&mut projected, run.provider_route.as_ref())?;
    Ok(projected)
}

/// Project progress from the same allowlisted provider-execution helper.
pub fn project_public_run_progress(
    store: &OrchStore,
    run: &RunRecord,
    busy: bool,
) -> Result<PublicRunProgress, OrchError> {
    let mut projected = PublicRunProgress {
        run_id: run.run_id.clone(),
        session_id: run.session_id,
        state: run.state,
        queue_position: run.queue_position,
        busy,
        start_seq: run.start_seq,
        end_seq: run.end_seq,
        prompt_preview: run.prompt_preview.clone(),
        progress: run.progress.as_ref().map(project_progress_detail),
        created_at: run.created_at,
        updated_at: run.updated_at,
        terminal_result: run.terminal_result.clone(),
        stop_cause: run.stop_cause,
        bounds: project_bounds(&run.bounds),
        error_code: run.error_code.clone(),
        provider_execution: project_provider_execution(store, run)?,
    };
    let mut as_run = PublicRun {
        run_id: projected.run_id.clone(),
        session_id: projected.session_id,
        workspace: run.workspace.clone(),
        request_id: run.request_id.clone(),
        client_id: run.client_id.clone(),
        state: projected.state,
        purpose: run.purpose,
        agent_id: run.agent_id.clone(),
        retry_of: run.retry_of.clone(),
        parent_run_id: run.parent_run_id.clone(),
        agent_spec_revision: run.agent_spec_revision,
        checkpoint_id: run.checkpoint_id.clone(),
        continuation_context_id: run.continuation_context_id.clone(),
        continuation_context_hash: run.continuation_context_hash.clone(),
        continuation_fidelity: run.continuation_fidelity.clone(),
        queue_position: projected.queue_position,
        bounds: projected.bounds.clone(),
        prompt_preview: projected.prompt_preview.clone(),
        start_seq: projected.start_seq,
        end_seq: projected.end_seq,
        created_at: projected.created_at,
        updated_at: projected.updated_at,
        terminal_result: projected.terminal_result.clone(),
        final_response: None,
        error_code: projected.error_code.clone(),
        stop_cause: projected.stop_cause,
        aggregates: PublicRunAggregates::default(),
        progress: projected.progress.clone(),
        execution: None,
        approval: None,
        provider_execution: projected.provider_execution.clone(),
    };
    scrub_public_run(&mut as_run, run.provider_route.as_ref())?;
    projected.prompt_preview = as_run.prompt_preview;
    projected.progress = as_run.progress;
    projected.terminal_result = as_run.terminal_result;
    projected.error_code = as_run.error_code;
    projected.provider_execution = as_run.provider_execution;
    Ok(projected)
}

pub fn page_public_runs(
    mut runs: Vec<PublicRun>,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> PublicRunPage {
    runs.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then(right.run_id.cmp(&left.run_id))
    });
    let total_count = runs.len();
    if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
        if let Some(index) = runs.iter().position(|run| run.run_id == cursor) {
            runs = runs.split_off(index.saturating_add(1));
        } else {
            runs.clear();
        }
    }
    let limit = limit
        .unwrap_or(PUBLIC_RUN_LIST_PAGE_LIMIT)
        .clamp(1, PUBLIC_RUN_LIST_PAGE_LIMIT);
    let truncated = runs.len() > limit;
    runs.truncate(limit);
    let next_cursor = truncated
        .then(|| runs.last().map(|run| run.run_id.clone()))
        .flatten();
    PublicRunPage {
        runs,
        total_count,
        truncated,
        next_cursor,
    }
}

pub fn public_run_to_value(run: &PublicRun) -> Result<Value, OrchError> {
    encode_allowlisted(run)
}

pub fn public_run_progress_to_value(progress: &PublicRunProgress) -> Result<Value, OrchError> {
    encode_allowlisted(progress)
}

/// Coordinator handoff built from the same scrubbed PublicRun as get/list.
pub fn public_run_handoff_value(run: &PublicRun) -> Result<Value, OrchError> {
    encode_allowlisted(&serde_json::json!({
        "runId": run.run_id,
        "sessionId": run.session_id,
        "state": run.state,
        "finalResponse": run.final_response,
        "terminalResult": run.terminal_result,
        "stopCause": run.stop_cause,
        "startSeq": run.start_seq,
        "endSeq": run.end_seq,
        "bounds": run.bounds,
        "changes": run.aggregates.changes,
        "tests": run.aggregates.tests,
        "verification": run.aggregates.verification,
        "usage": run.aggregates.usage,
        "usageComplete": run.aggregates.usage_complete,
        "usagePendingRequests": run.aggregates.usage_pending_requests,
        "errorCode": run.error_code,
    }))
}

pub fn public_run_contains_forbidden_fields(value: &Value) -> bool {
    contains_forbidden_key(value)
}

/// True when `providerExecution.route` contains only the public allowlist.
pub fn public_provider_route_keys_are_allowlisted(route: &Value) -> bool {
    let Some(object) = route.as_object() else {
        return false;
    };
    object
        .keys()
        .all(|key| PUBLIC_PROVIDER_ROUTE_KEYS.contains(&key.as_str()))
}

/// Store a versioned PublicRun receipt. The MCP response remains the inner run.
pub fn encode_public_run_receipt(run: &PublicRun) -> Result<Value, OrchError> {
    Ok(serde_json::json!({
        "schema": PUBLIC_RUN_RECEIPT_SCHEMA,
        "run": public_run_to_value(run)?,
    }))
}

/// Replay a promote/discard receipt. Legacy leaky RunRecord JSON is re-projected
/// from the durable store and never returned as stored.
pub fn public_run_from_receipt(store: &OrchStore, value: Value) -> Result<Value, OrchError> {
    if value.get("schema").and_then(Value::as_str) == Some(PUBLIC_RUN_RECEIPT_SCHEMA) {
        let receipt: PublicRunReceiptV1 = serde_json::from_value(value).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("public run receipt is not exact: {error}"),
            )
        })?;
        if receipt.schema != PUBLIC_RUN_RECEIPT_SCHEMA {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "public run receipt schema is invalid",
            ));
        }
        return public_run_to_value(&receipt.run);
    }
    if public_run_contains_forbidden_fields(&value) || value.get("providerRoute").is_some() {
        let run_id = value
            .get("runId")
            .or_else(|| value.get("run_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    "legacy run receipt is missing runId",
                )
            })?;
        let run = store
            .load_run(run_id)
            .map_err(|_| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    "legacy run receipt could not load its durable Run",
                )
            })?
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    "legacy run receipt names an unknown Run",
                )
            })?;
        return public_run_to_value(&project_public_run(store, &run)?);
    }
    encode_decoded_public_run(value)
}

fn encode_decoded_public_run(value: Value) -> Result<Value, OrchError> {
    let parsed: PublicRun = serde_json::from_value(value).map_err(|error| {
        OrchError::new(
            OrchErrorCode::Internal,
            format!("public run receipt is not a PublicRun: {error}"),
        )
    })?;
    public_run_to_value(&parsed)
}

fn encode_allowlisted<T: Serialize>(value: &T) -> Result<Value, OrchError> {
    let encoded = serde_json::to_value(value)
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
    if contains_forbidden_key(&encoded) || contains_forbidden_value(&encoded, &[]) {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "public run projection refused to serialize",
        ));
    }
    Ok(encoded)
}

fn contains_forbidden_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys().any(|key| {
                FORBIDDEN_PUBLIC_RUN_KEYS
                    .iter()
                    .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
            }) || map.values().any(contains_forbidden_key)
        }
        Value::Array(values) => values.iter().any(contains_forbidden_key),
        _ => false,
    }
}

fn contains_forbidden_value(value: &Value, extra_needles: &[&str]) -> bool {
    match value {
        Value::Object(map) => map
            .values()
            .any(|child| contains_forbidden_value(child, extra_needles)),
        Value::Array(values) => values
            .iter()
            .any(|child| contains_forbidden_value(child, extra_needles)),
        Value::String(text) => extra_needles
            .iter()
            .any(|needle| !needle.is_empty() && text.contains(needle)),
        _ => false,
    }
}

fn project_provider_execution(
    store: &OrchStore,
    run: &RunRecord,
) -> Result<Option<PublicProviderExecution>, OrchError> {
    let Some(route) = run.provider_route.as_ref() else {
        return Ok(None);
    };
    route.validate()?;
    let quota = match route.quota_reservation_id.as_deref() {
        Some(reservation_id) => {
            let reservation = store
                .load_quota_reservation(reservation_id)
                .map_err(provider_projection_error)?
                .ok_or_else(|| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        "provider quota reservation is missing",
                    )
                })?;
            if reservation.run_id != run.run_id
                || reservation.route_snapshot_hash != route.snapshot_hash
                || reservation.pool.provider_id != route.provider_id
            {
                return Err(OrchError::new(
                    OrchErrorCode::Internal,
                    "provider quota reservation does not match the Run",
                ));
            }
            Some(project_quota(&reservation))
        }
        None => None,
    };
    let page = store
        .list_provider_attempts_for_run(&run.run_id)
        .map_err(provider_projection_error)?;
    Ok(Some(PublicProviderExecution {
        route: project_route_summary(route),
        quota,
        attempts: page.attempts.into_iter().map(project_attempt).collect(),
        attempt_count: page.total_count,
        attempts_truncated: page.truncated,
        usage_complete: run.aggregates.usage_complete,
        pending_requests: run.aggregates.usage_pending_requests,
    }))
}

fn project_route_summary(route: &ProviderRouteSnapshot) -> PublicProviderRouteSummary {
    PublicProviderRouteSummary {
        provider_id: route.provider_id.clone(),
        kind: route.kind,
        dialect: route.dialect,
        model_id: route.model_id.clone(),
        wire_model_id: route.wire_model_id.clone(),
        capability_source: route.capabilities.source,
        qualification_schema: route.capabilities.qualification_schema.clone(),
        deadline_class: route.deadline_class,
        effort: route.effort,
        snapshot_hash: route.snapshot_hash.clone(),
    }
}

fn project_quota(reservation: &QuotaReservation) -> PublicProviderQuota {
    PublicProviderQuota {
        reservation_id: reservation.reservation_id.clone(),
        pool_id: reservation.pool_id.clone(),
        class: reservation.pool.class,
        state: reservation.state,
        tokens_reserved: reservation.tokens_reserved,
        tokens_consumed: reservation.tokens_consumed,
        requests_reserved: reservation.requests_reserved,
        requests_consumed: reservation.requests_consumed,
        window_started_at: reservation.window_started_at,
        limits: PublicQuotaLimits {
            window_ms: reservation.limits.window_ms,
            max_in_flight_reservations: reservation.limits.max_in_flight_reservations,
            max_tokens_per_window: reservation.limits.max_tokens_per_window,
            max_requests_per_window: reservation.limits.max_requests_per_window,
        },
        updated_at: reservation.updated_at,
    }
}

fn project_attempt(attempt: ProviderAttemptRecord) -> PublicProviderAttempt {
    PublicProviderAttempt {
        attempt_id: attempt.attempt_id,
        ordinal: attempt.ordinal,
        state: attempt.state,
        send_certainty: attempt.send_certainty,
        retry_class: attempt.retry_class,
        http_status: attempt.http_status,
        usage: attempt.usage.as_ref().map(PublicCompletionUsage::from),
        created_at: attempt.created_at,
        updated_at: attempt.updated_at,
        finished_at: attempt.finished_at,
    }
}

fn project_bounds(bounds: &RunBounds) -> PublicRunBounds {
    PublicRunBounds {
        max_prompt_bytes: bounds.max_prompt_bytes,
        max_rounds: bounds.max_rounds,
        max_duration_ms: bounds.max_duration_ms,
        max_total_tokens: bounds.max_total_tokens,
    }
}

fn project_aggregates(aggregates: &RunAggregates) -> PublicRunAggregates {
    PublicRunAggregates {
        changes: aggregates
            .changes
            .iter()
            .map(|change| PublicChangeRecord {
                path: change.path.clone(),
                summary: change.summary.clone(),
            })
            .collect(),
        tests: aggregates
            .tests
            .iter()
            .map(|test| PublicTestObservation {
                call_id: test.call_id.clone(),
                command: test.command.clone(),
                status: test.status.clone(),
                exit_code: test.exit_code,
                cancelled: test.cancelled,
            })
            .collect(),
        permissions_requested: aggregates.permissions_requested,
        permissions_granted: aggregates.permissions_granted,
        permissions_denied: aggregates.permissions_denied,
        usage: PublicCompletionUsage::from(&aggregates.usage),
        usage_complete: aggregates.usage_complete,
        usage_pending_requests: aggregates.usage_pending_requests,
        verification: aggregates
            .verification
            .as_ref()
            .map(PublicCompletionEvidence::from),
    }
}

fn project_progress_detail(progress: &RunProgress) -> PublicRunProgressDetail {
    PublicRunProgressDetail {
        round: progress.round,
        max_rounds: progress.max_rounds,
        last_tool: progress.last_tool.clone(),
        detail: progress.detail.clone(),
        updated_at: progress.updated_at,
    }
}

fn project_execution(execution: &RunExecution) -> PublicRunExecution {
    PublicRunExecution {
        mode: execution.mode,
        source_workspace: execution.source_workspace.clone(),
        execution_workspace: execution.execution_workspace.clone(),
        base_revision: execution.base_revision.clone(),
        source_fingerprint: execution.source_fingerprint.clone(),
        final_fingerprint: execution.final_fingerprint.clone(),
        promotion_state: execution.promotion_state,
        promoted_at: execution.promoted_at,
    }
}

fn project_approval(approval: &RunApproval) -> PublicRunApproval {
    PublicRunApproval {
        approval_id: approval.approval_id.clone(),
        run_id: approval.run_id.clone(),
        session_id: approval.session_id,
        workspace: approval.workspace.clone(),
        source_fingerprint: approval.source_fingerprint.clone(),
        final_fingerprint: approval.final_fingerprint.clone(),
        changed_files: approval
            .changed_files
            .iter()
            .map(|change| PublicChangeRecord {
                path: change.path.clone(),
                summary: change.summary.clone(),
            })
            .collect(),
        issued_at: approval.issued_at,
        expires_at: approval.expires_at,
    }
}

fn route_secret_needles(route: Option<&ProviderRouteSnapshot>) -> Vec<String> {
    let Some(route) = route else {
        return Vec::new();
    };
    [
        route.base_url.as_str(),
        route.credential_ref.as_str(),
        route.credential_fingerprint.as_str(),
        route.endpoint_fingerprint.as_str(),
        route.selection_key.as_str(),
        route.qualification_record_id.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .filter(|needle| !needle.is_empty())
    .map(str::to_string)
    .collect()
}

fn scrub_public_run(
    run: &mut PublicRun,
    route: Option<&ProviderRouteSnapshot>,
) -> Result<(), OrchError> {
    let needles = route_secret_needles(route);
    if needles.is_empty() {
        return Ok(());
    }
    let mut redacted = false;
    redacted |= scrub_string(&mut run.prompt_preview, &needles);
    redacted |= scrub_opt_string(&mut run.final_response, &needles);
    redacted |= scrub_opt_string(&mut run.terminal_result, &needles);
    if let Some(progress) = run.progress.as_mut() {
        redacted |= scrub_string(&mut progress.detail, &needles);
        if let Some(tool) = progress.last_tool.as_mut() {
            redacted |= scrub_string(tool, &needles);
        }
    }
    for change in &mut run.aggregates.changes {
        redacted |= scrub_string(&mut change.path, &needles);
        redacted |= scrub_string(&mut change.summary, &needles);
    }
    for test in &mut run.aggregates.tests {
        redacted |= scrub_opt_string(&mut test.command, &needles);
        redacted |= scrub_string(&mut test.status, &needles);
    }
    if let Some(execution) = run.execution.as_mut() {
        redacted |= scrub_string(&mut execution.source_workspace, &needles);
        redacted |= scrub_string(&mut execution.execution_workspace, &needles);
    }
    if redacted {
        run.error_code = Some(PUBLIC_ERROR_PRIVILEGED_DIAGNOSTICS.into());
    }
    let needle_refs: Vec<&str> = needles.iter().map(String::as_str).collect();
    if let Some(evidence) = run.aggregates.verification.as_ref() {
        let encoded = serde_json::to_value(evidence)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if contains_forbidden_value(&encoded, &needle_refs) {
            run.aggregates.verification = None;
            run.error_code = Some(PUBLIC_ERROR_PRIVILEGED_DIAGNOSTICS.into());
        }
    }
    let encoded = serde_json::to_value(&*run)
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
    let needle_refs: Vec<&str> = needles.iter().map(String::as_str).collect();
    if contains_forbidden_value(&encoded, &needle_refs) {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "public run projection refused to serialize privileged diagnostics",
        ));
    }
    Ok(())
}

fn scrub_string(value: &mut String, needles: &[String]) -> bool {
    if needles.iter().any(|needle| value.contains(needle)) {
        *value = String::new();
        true
    } else {
        false
    }
}

fn scrub_opt_string(value: &mut Option<String>, needles: &[String]) -> bool {
    let Some(text) = value.as_mut() else {
        return false;
    };
    if needles.iter().any(|needle| text.contains(needle)) {
        *value = None;
        true
    } else {
        false
    }
}

fn provider_projection_error(_error: anyhow::Error) -> OrchError {
    OrchError::new(
        OrchErrorCode::Internal,
        "provider execution ledger is unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_config::{
        model_selection_key, ModelCapabilities, CAPABILITY_QUALIFICATION_SCHEMA,
    };
    use crate::orchestration::quota::QuotaLimits;
    use crate::orchestration::{
        QuotaClass, QuotaReservation, PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
    };
    use chrono::Utc;
    use serde_json::json;
    use tempfile::tempdir;

    const BASE_URL_SENTINEL: &str = "http://127.0.0.1:35201/leak-base-url-sentinel-pr352/v1";
    const CREDENTIAL_REF_SENTINEL: &str = "keychain:provider/leak-cred-ref-sentinel-pr352";
    const CREDENTIAL_FP_SENTINEL: &str = "v1-sha256:leak-cred-fp-sentinel-pr352";
    const MODEL_ID: &str = "leak-model";
    const QUOTA_RESERVATION_SENTINEL: &str = "quota-leak-reservation-sentinel-pr352";

    fn leaky_route() -> ProviderRouteSnapshot {
        ProviderRouteSnapshot {
            schema_version: PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
            provider_id: "company-gateway".into(),
            model_id: MODEL_ID.into(),
            wire_model_id: MODEL_ID.into(),
            selection_key: model_selection_key("company-gateway", MODEL_ID),
            kind: ProviderKind::OpenAiCompatible,
            dialect: ProviderDialect::OpenAiChatCompletions,
            base_url: BASE_URL_SENTINEL.into(),
            endpoint_fingerprint: String::new(),
            credential_ref: CREDENTIAL_REF_SENTINEL.into(),
            credential_fingerprint: CREDENTIAL_FP_SENTINEL.into(),
            capabilities: ModelCapabilities {
                chat: true,
                tools: true,
                stream: true,
                source: CapabilitySource::Measured,
                qualification_schema: Some(CAPABILITY_QUALIFICATION_SCHEMA.into()),
                ..ModelCapabilities::default()
            },
            deadline_class: ProviderDeadlineClass::Standard,
            effort: EffortLevel::Medium,
            qualification_record_id: None,
            quota_class: None,
            quota_reservation_id: None,
            snapshot_hash: String::new(),
        }
        .seal()
        .unwrap()
        .bind_quota(QuotaClass::CodingExecution, QUOTA_RESERVATION_SENTINEL)
        .unwrap()
    }

    fn leaky_run(route: ProviderRouteSnapshot) -> RunRecord {
        let now = Utc::now();
        RunRecord {
            run_id: "public-run-leak".into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/public-run".into(),
            request_id: "public-run-req".into(),
            client_id: Some("mcp".into()),
            state: RunState::Running,
            purpose: RunPurpose::Execution,
            provider_route: Some(route),
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds {
                max_total_tokens: Some(8_000),
                ..RunBounds::default()
            },
            prompt_preview: "inspect".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        }
    }

    fn assert_payload_hides_route(payload: &Value, route: &ProviderRouteSnapshot) {
        fn walk(value: &Value, path: &str) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        assert!(
                            !key.eq_ignore_ascii_case("providerRoute")
                                && !key.eq_ignore_ascii_case("provider_route"),
                            "providerRoute leaked at {path}.{key}"
                        );
                        walk(child, &format!("{path}.{key}"));
                    }
                }
                Value::Array(values) => {
                    for (index, child) in values.iter().enumerate() {
                        walk(child, &format!("{path}[{index}]"));
                    }
                }
                _ => {}
            }
        }
        walk(payload, "$");
        let encoded = payload.to_string();
        let qualification = route.qualification_record_id.as_deref().unwrap_or_default();
        for sentinel in [
            route.base_url.as_str(),
            route.credential_ref.as_str(),
            route.credential_fingerprint.as_str(),
            route.endpoint_fingerprint.as_str(),
            qualification,
            route.selection_key.as_str(),
        ] {
            if sentinel.is_empty() {
                continue;
            }
            assert!(
                !encoded.contains(sentinel),
                "public payload leaked {sentinel}: {encoded}"
            );
        }
        assert!(!public_run_contains_forbidden_fields(payload));
        let route_json = payload
            .pointer("/providerExecution/route")
            .or_else(|| payload.pointer("/run/providerExecution/route"))
            .or_else(|| payload.pointer("/runs/0/providerExecution/route"))
            .expect("public payload must include providerExecution.route");
        assert!(route_json.get("quotaReservationId").is_none());
        assert!(route_json.get("selectionKey").is_none());
        assert!(route_json.get("qualificationRecordId").is_none());
        assert!(
            public_provider_route_keys_are_allowlisted(route_json),
            "providerExecution.route keys must be exact-allowlisted: {route_json}"
        );
        let quota_id = route
            .quota_reservation_id
            .as_deref()
            .unwrap_or(QUOTA_RESERVATION_SENTINEL);
        if let Some(object) = payload.as_object() {
            if let Some(quota) = object
                .get("providerExecution")
                .and_then(|value| value.get("quota"))
            {
                assert_eq!(quota["reservationId"], quota_id);
            }
        }
    }

    #[test]
    fn public_projection_omits_frozen_route_and_unique_sentinels() {
        let temp = tempdir().unwrap();
        let store = OrchStore::open(temp.path()).unwrap();
        let route = leaky_route();
        let run = leaky_run(route.clone());
        let reservation =
            QuotaReservation::for_run(&run, "owner-a", QuotaLimits::default(), Utc::now()).unwrap();
        store
            .admit_run_with_quota(&run, &reservation)
            .into_result()
            .unwrap();
        let projected =
            project_public_run(&store, &store.load_run(&run.run_id).unwrap().unwrap()).unwrap();
        let get_payload = public_run_to_value(&projected).unwrap();
        let list_payload = json!({ "runs": [get_payload.clone()] });
        let progress = public_run_progress_to_value(
            &project_public_run_progress(
                &store,
                &store.load_run(&run.run_id).unwrap().unwrap(),
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let persisted =
            serde_json::to_value(store.load_run(&run.run_id).unwrap().unwrap()).unwrap();
        assert!(persisted.get("providerRoute").is_some());
        for payload in [&get_payload, &list_payload["runs"][0], &progress] {
            assert_payload_hides_route(payload, run.provider_route.as_ref().unwrap());
            assert_eq!(
                payload["providerExecution"]["route"]["snapshotHash"],
                run.provider_route.as_ref().unwrap().snapshot_hash
            );
        }
        let decoded: PublicRun = serde_json::from_value(get_payload.clone()).unwrap();
        assert_payload_hides_route(
            &serde_json::to_value(&decoded).unwrap(),
            run.provider_route.as_ref().unwrap(),
        );
        let receipt = encode_public_run_receipt(&projected).unwrap();
        assert_eq!(receipt["schema"], PUBLIC_RUN_RECEIPT_SCHEMA);
        assert_payload_hides_route(&receipt, run.provider_route.as_ref().unwrap());
        let replayed = public_run_from_receipt(&store, receipt).unwrap();
        assert_payload_hides_route(&replayed, run.provider_route.as_ref().unwrap());
        let leaked_receipt = persisted.clone();
        assert!(leaked_receipt.get("providerRoute").is_some());
        let sanitized = public_run_from_receipt(&store, leaked_receipt).unwrap();
        assert_payload_hides_route(&sanitized, run.provider_route.as_ref().unwrap());
        assert_eq!(sanitized["runId"], run.run_id);
    }

    #[test]
    fn reintroducing_raw_run_record_serialization_fails() {
        let service = include_str!("service.rs");
        let list_runs = service
            .split("pub fn list_runs_scoped_page")
            .nth(1)
            .expect("list_runs_scoped_page")
            .split("// ── durable workloads")
            .next()
            .unwrap();
        let run_value = service
            .split("fn run_value(")
            .nth(1)
            .expect("run_value")
            .split("\n    pub fn ")
            .next()
            .unwrap();
        let progress_value = service
            .split("fn progress_value(")
            .nth(1)
            .expect("progress_value")
            .split("\n    fn ")
            .next()
            .unwrap();
        assert!(
            !run_value.contains("serde_json::to_value(run)"),
            "get_run must project PublicRun instead of serializing RunRecord"
        );
        assert!(
            run_value.contains("project_public_run"),
            "get_run must call project_public_run"
        );
        assert!(
            list_runs.contains("list_runs_for_session_page"),
            "list_runs must use the session-bounded store query"
        );
        assert!(
            !list_runs.contains(".list_runs()"),
            "list_runs must not scan the global Run directory"
        );
        assert!(
            list_runs.contains("project_public_run"),
            "list_runs must call project_public_run"
        );
        assert!(
            list_runs.contains("public_run_to_value"),
            "list_runs must encode PublicRun values"
        );
        assert!(
            !list_runs.contains("serde_json::to_value(run)"),
            "list_runs must not serialize raw RunRecord values"
        );
        assert!(
            progress_value.contains("project_public_run_progress"),
            "progress must use the shared public progress projection"
        );
        assert!(
            !progress_value.contains("serde_json::to_value(run)"),
            "progress must not serialize RunRecord"
        );
        let promote = service
            .split("pub async fn promote_run(")
            .nth(1)
            .expect("promote_run")
            .split("\n    pub async fn ")
            .next()
            .unwrap();
        let discard = service
            .split("pub async fn discard_run(")
            .nth(1)
            .expect("discard_run")
            .split("\n    // ── mutations")
            .next()
            .unwrap();
        assert!(
            promote.contains("project_public_run") && promote.contains("public_run_from_receipt"),
            "promote must project PublicRun and sanitize replay receipts"
        );
        assert!(
            !promote.contains("serde_json::to_value(promoted)"),
            "promote must not serialize raw RunRecord"
        );
        assert!(
            discard.contains("project_public_run") && discard.contains("public_run_from_receipt"),
            "discard must project PublicRun and sanitize replay receipts"
        );
        assert!(
            !discard.contains("serde_json::to_value(discarded)"),
            "discard must not serialize raw RunRecord"
        );

        let host = include_str!("../host.rs");
        let public_list = host
            .split("pub fn list_public_session_runs_page")
            .nth(1)
            .expect("list_public_session_runs_page")
            .split("\n    pub fn ")
            .next()
            .unwrap();
        let public_get = host
            .split("pub fn get_public_session_run")
            .nth(1)
            .expect("get_public_session_run")
            .split("\n    pub fn ")
            .next()
            .unwrap();
        assert!(
            public_list.contains("list_runs_for_session_page"),
            "desktop list must use the session-bounded store query"
        );
        assert!(
            public_list.contains("project_public_session_run"),
            "desktop list must project PublicRun"
        );
        assert!(
            public_get.contains("project_public_session_run"),
            "desktop get must project PublicRun"
        );
        assert!(
            !public_list.contains("serde_json::to_value"),
            "desktop list must not serialize RunRecord"
        );
        assert!(
            !public_get.contains("serde_json::to_value"),
            "desktop get must not serialize RunRecord"
        );
    }

    #[test]
    fn nested_public_run_dtos_are_exact_and_deny_unknown_fields() {
        let temp = tempdir().unwrap();
        let store = OrchStore::open(temp.path()).unwrap();
        let run = leaky_run(leaky_route());
        let reservation =
            QuotaReservation::for_run(&run, "owner-a", QuotaLimits::default(), Utc::now()).unwrap();
        store
            .admit_run_with_quota(&run, &reservation)
            .into_result()
            .unwrap();
        let projected =
            project_public_run(&store, &store.load_run(&run.run_id).unwrap().unwrap()).unwrap();
        let encoded = public_run_to_value(&projected).unwrap();
        let decoded: PublicRun = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded.run_id, projected.run_id);
        assert!(encoded.get("providerRoute").is_none());
        assert!(encoded["aggregates"]
            .get("accountedProviderAttemptIds")
            .is_none());
        let mut with_unknown_bounds = encoded.clone();
        with_unknown_bounds["bounds"]["leakedBaseUrl"] = json!(BASE_URL_SENTINEL);
        assert!(
            serde_json::from_value::<PublicRun>(with_unknown_bounds).is_err(),
            "unknown nested bounds keys must fail"
        );
        let mut with_unknown_route = encoded.clone();
        with_unknown_route["providerExecution"]["route"]["selectionKey"] =
            json!("ptah.model.v1:leaked");
        assert!(
            serde_json::from_value::<PublicRun>(with_unknown_route).is_err(),
            "unknown nested providerExecution.route keys must fail"
        );
        let mut with_unknown_aggregates = encoded.clone();
        with_unknown_aggregates["aggregates"]["internalFence"] = json!(["attempt-1"]);
        assert!(
            serde_json::from_value::<PublicRun>(with_unknown_aggregates).is_err(),
            "unknown nested aggregates keys must fail"
        );
        let mut with_unknown_usage = encoded.clone();
        with_unknown_usage["aggregates"]["usage"]["selectionKey"] = json!("ptah.model.v1:leaked");
        assert!(
            serde_json::from_value::<PublicRun>(with_unknown_usage).is_err(),
            "unknown nested aggregates.usage keys must fail"
        );
        let mut with_unknown_verification = encoded.clone();
        with_unknown_verification["aggregates"]["verification"] = json!({
            "status": "unverified",
            "stopReason": "completed",
            "interrupted": false,
            "claims": {
                "present": false,
                "mentionsChanges": false,
                "mentionsTests": false,
                "mentionsVerification": false,
                "credentialRef": CREDENTIAL_REF_SENTINEL
            },
            "observations": {
                "changedFiles": 0,
                "testsObserved": 0,
                "testsPassed": 0,
                "testsFailed": 0,
                "testsIncomplete": 0,
                "permissionsRequested": 0,
                "permissionsGranted": 0,
                "permissionsDenied": 0,
                "permissionsUnresolved": 0
            },
            "usage": {
                "promptTokens": 0,
                "completionTokens": 0,
                "totalTokens": 0,
                "requests": 0
            }
        });
        assert!(
            serde_json::from_value::<PublicRun>(with_unknown_verification).is_err(),
            "unknown nested verification.claims keys must fail"
        );
        let mut with_unknown_attempt_usage = encoded.clone();
        with_unknown_attempt_usage["providerExecution"]["attempts"] = json!([{
            "attemptId": "attempt-1",
            "ordinal": 1,
            "state": "admitted",
            "sendCertainty": null,
            "retryClass": null,
            "httpStatus": null,
            "usage": {
                "promptTokens": 0,
                "completionTokens": 0,
                "totalTokens": 0,
                "requests": 0,
                "baseUrl": BASE_URL_SENTINEL
            },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "finishedAt": null
        }]);
        assert!(
            serde_json::from_value::<PublicRun>(with_unknown_attempt_usage).is_err(),
            "unknown nested providerExecution.attempts.usage keys must fail"
        );
    }

    #[test]
    fn failing_provider_values_are_scrubbed_on_get_list_progress_promote_discard_and_replay() {
        let temp = tempdir().unwrap();
        let store = OrchStore::open(temp.path()).unwrap();
        let route = leaky_route();
        let mut run = leaky_run(route.clone());
        run.final_response = Some(format!("failed calling {}", BASE_URL_SENTINEL));
        run.terminal_result = Some(format!("credential {} rejected", CREDENTIAL_REF_SENTINEL));
        run.prompt_preview = format!("retry {}", route.selection_key);
        run.progress = Some(crate::orchestration::types::RunProgress {
            round: 1,
            max_rounds: 4,
            last_tool: Some(format!("mcp:{}", CREDENTIAL_FP_SENTINEL)),
            detail: format!("upstream {} fingerprint", route.endpoint_fingerprint),
            updated_at: Utc::now(),
        });
        run.aggregates
            .changes
            .push(crate::orchestration::types::ChangeRecord {
                path: format!("called {}", BASE_URL_SENTINEL),
                summary: format!("ref {}", CREDENTIAL_REF_SENTINEL),
            });
        let reservation =
            QuotaReservation::for_run(&run, "owner-a", QuotaLimits::default(), Utc::now()).unwrap();
        store
            .admit_run_with_quota(&run, &reservation)
            .into_result()
            .unwrap();
        let loaded = store.load_run(&run.run_id).unwrap().unwrap();
        let projected = project_public_run(&store, &loaded).unwrap();
        let get_payload = public_run_to_value(&projected).unwrap();
        let list_page = page_public_runs(vec![projected.clone()], None, None);
        let list_payload = json!({
            "runs": list_page.runs.iter().map(public_run_to_value).collect::<Result<Vec<_>, _>>().unwrap(),
            "totalCount": list_page.total_count,
            "truncated": list_page.truncated,
        });
        let progress = public_run_progress_to_value(
            &project_public_run_progress(&store, &loaded, false).unwrap(),
        )
        .unwrap();
        let handoff = public_run_handoff_value(&projected).unwrap();
        let receipt = encode_public_run_receipt(&projected).unwrap();
        let replayed = public_run_from_receipt(&store, receipt.clone()).unwrap();
        let legacy = serde_json::to_value(&loaded).unwrap();
        let legacy_replayed = public_run_from_receipt(&store, legacy).unwrap();
        for (name, payload) in [
            ("get", &get_payload),
            ("list", &list_payload["runs"][0]),
            ("progress", &progress),
            ("promote-receipt", &receipt["run"]),
            ("discard-replay", &replayed),
            ("legacy-replay", &legacy_replayed),
        ] {
            assert_payload_hides_route(payload, &route);
            let encoded = payload.to_string();
            let pretty = serde_json::to_string_pretty(payload).unwrap();
            for haystack in [&encoded, &pretty] {
                assert!(
                    !haystack.contains(BASE_URL_SENTINEL),
                    "{name} MCP text leaked base_url: {haystack}"
                );
                assert!(
                    !haystack.contains(CREDENTIAL_REF_SENTINEL),
                    "{name} MCP text leaked credential ref"
                );
            }
            assert_eq!(
                payload["errorCode"], PUBLIC_ERROR_PRIVILEGED_DIAGNOSTICS,
                "{name} must use the typed public error code"
            );
        }
        let handoff_encoded = handoff.to_string();
        assert!(
            !handoff_encoded.contains(BASE_URL_SENTINEL)
                && !handoff_encoded.contains(CREDENTIAL_REF_SENTINEL),
            "handoff MCP text leaked route secrets: {handoff_encoded}"
        );
        assert!(handoff.get("providerRoute").is_none());
        assert!(handoff.get("providerExecution").is_none());
        assert_eq!(
            handoff["errorCode"], PUBLIC_ERROR_PRIVILEGED_DIAGNOSTICS,
            "handoff must use the typed public error code"
        );
        assert!(handoff["finalResponse"].is_null());
        assert!(handoff["terminalResult"].is_null());
        assert!(projected.final_response.is_none());
        assert!(projected.terminal_result.is_none());
    }

    #[test]
    fn public_run_list_caps_at_128_with_exact_truncation_count() {
        let now = Utc::now();
        let runs = (0..200)
            .map(|index| PublicRun {
                run_id: format!("run-{index:03}"),
                session_id: Uuid::nil(),
                workspace: "/tmp/public-run".into(),
                request_id: format!("req-{index}"),
                client_id: None,
                state: RunState::Completed,
                purpose: RunPurpose::Execution,
                agent_id: None,
                retry_of: None,
                parent_run_id: None,
                agent_spec_revision: None,
                checkpoint_id: None,
                continuation_context_id: None,
                continuation_context_hash: None,
                continuation_fidelity: None,
                queue_position: None,
                bounds: PublicRunBounds {
                    max_prompt_bytes: 1,
                    max_rounds: 1,
                    max_duration_ms: 1,
                    max_total_tokens: None,
                },
                prompt_preview: "x".into(),
                start_seq: None,
                end_seq: None,
                created_at: now,
                updated_at: now,
                terminal_result: None,
                final_response: None,
                error_code: None,
                stop_cause: None,
                aggregates: PublicRunAggregates::default(),
                progress: None,
                execution: None,
                approval: None,
                provider_execution: None,
            })
            .collect();
        let page = page_public_runs(runs, None, None);
        assert_eq!(page.total_count, 200);
        assert!(page.truncated);
        assert_eq!(page.runs.len(), PUBLIC_RUN_LIST_PAGE_LIMIT);
        assert_eq!(page.next_cursor.as_deref(), Some("run-072"));
    }
}
