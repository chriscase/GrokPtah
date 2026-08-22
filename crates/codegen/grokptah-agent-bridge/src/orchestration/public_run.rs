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

use crate::completion::CompletionUsage;
use crate::gateway_config::{
    CapabilitySource, ProviderDeadlineClass, ProviderDialect, ProviderKind,
};
use crate::types::EffortLevel;

use super::provider_attempt::{
    ProviderAttemptRecord, ProviderAttemptState, ProviderRetryClass, ProviderSendCertainty,
};
use super::quota::{QuotaClass, QuotaLimits, QuotaReservation, QuotaReservationState};
use super::store::OrchStore;
use super::types::{
    OrchError, OrchErrorCode, ProviderRouteSnapshot, RunAggregates, RunApproval, RunBounds,
    RunExecution, RunProgress, RunPurpose, RunRecord, RunState, RunStopCause,
};

const MAX_PROJECTED_PROVIDER_ATTEMPTS: usize = 128;

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

/// Secret-free route identity that operators and coordinators may observe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// Quota row joined into the public provider-execution summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    pub limits: QuotaLimits,
    pub updated_at: DateTime<Utc>,
}

/// One durable provider attempt in the public summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicProviderAttempt {
    pub attempt_id: String,
    pub ordinal: u64,
    pub state: ProviderAttemptState,
    pub send_certainty: Option<ProviderSendCertainty>,
    pub retry_class: Option<ProviderRetryClass>,
    pub http_status: Option<u16>,
    pub usage: Option<CompletionUsage>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Bounded provider-execution overlay shared by list, get, and progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicProviderExecution {
    pub route: PublicProviderRouteSummary,
    pub quota: Option<PublicProviderQuota>,
    pub attempts: Vec<PublicProviderAttempt>,
    pub attempt_count: usize,
    pub attempts_truncated: bool,
    pub usage_complete: bool,
    pub pending_requests: u32,
}

/// Allowlisted public Run. This is the only Run shape MCP, hosted service,
/// local Tauri, and remote-desktop decoding may serialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    pub bounds: RunBounds,
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
    pub aggregates: RunAggregates,
    #[serde(default)]
    pub progress: Option<RunProgress>,
    #[serde(default)]
    pub execution: Option<RunExecution>,
    #[serde(default)]
    pub approval: Option<RunApproval>,
    #[serde(default)]
    pub provider_execution: Option<PublicProviderExecution>,
}

/// Allowlisted progress view. Built from the same provider-execution helper
/// as [`PublicRun`]; it never serializes a [`RunRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicRunProgress {
    pub run_id: String,
    pub session_id: Uuid,
    pub state: RunState,
    pub queue_position: Option<usize>,
    pub busy: bool,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
    pub prompt_preview: String,
    pub progress: Option<RunProgress>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminal_result: Option<String>,
    pub stop_cause: Option<RunStopCause>,
    pub bounds: RunBounds,
    pub error_code: Option<String>,
    pub provider_execution: Option<PublicProviderExecution>,
}

/// Project one persisted Run into the public allowlist.
pub fn project_public_run(store: &OrchStore, run: &RunRecord) -> Result<PublicRun, OrchError> {
    let provider_execution = project_provider_execution(store, run)?;
    Ok(PublicRun {
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
        bounds: run.bounds.clone(),
        prompt_preview: run.prompt_preview.clone(),
        start_seq: run.start_seq,
        end_seq: run.end_seq,
        created_at: run.created_at,
        updated_at: run.updated_at,
        terminal_result: run.terminal_result.clone(),
        final_response: run.final_response.clone(),
        error_code: run.error_code.clone(),
        stop_cause: run.stop_cause,
        aggregates: run.aggregates.clone(),
        progress: run.progress.clone(),
        execution: run.execution.clone(),
        approval: run.approval.clone(),
        provider_execution,
    })
}

/// Project progress from the same allowlisted provider-execution helper.
pub fn project_public_run_progress(
    store: &OrchStore,
    run: &RunRecord,
    busy: bool,
) -> Result<PublicRunProgress, OrchError> {
    Ok(PublicRunProgress {
        run_id: run.run_id.clone(),
        session_id: run.session_id,
        state: run.state,
        queue_position: run.queue_position,
        busy,
        start_seq: run.start_seq,
        end_seq: run.end_seq,
        prompt_preview: run.prompt_preview.clone(),
        progress: run.progress.clone(),
        created_at: run.created_at,
        updated_at: run.updated_at,
        terminal_result: run.terminal_result.clone(),
        stop_cause: run.stop_cause,
        bounds: run.bounds.clone(),
        error_code: run.error_code.clone(),
        provider_execution: project_provider_execution(store, run)?,
    })
}

pub fn public_run_to_value(run: &PublicRun) -> Result<Value, OrchError> {
    encode_allowlisted(run)
}

pub fn public_run_progress_to_value(progress: &PublicRunProgress) -> Result<Value, OrchError> {
    encode_allowlisted(progress)
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
        let run = value.get("run").cloned().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::Internal,
                "versioned public run receipt is missing its run",
            )
        })?;
        return encode_decoded_public_run(run);
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
    if contains_forbidden_key(&encoded) {
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
    let mut attempts = store
        .list_provider_attempts()
        .map_err(provider_projection_error)?
        .into_iter()
        .filter(|attempt| attempt.run_id == run.run_id)
        .collect::<Vec<_>>();
    attempts.sort_by(|left, right| {
        left.ordinal
            .cmp(&right.ordinal)
            .then(left.attempt_id.cmp(&right.attempt_id))
    });
    let attempt_count = attempts.len();
    let truncated = attempt_count > MAX_PROJECTED_PROVIDER_ATTEMPTS;
    attempts.truncate(MAX_PROJECTED_PROVIDER_ATTEMPTS);
    Ok(Some(PublicProviderExecution {
        route: project_route_summary(route),
        quota,
        attempts: attempts.into_iter().map(project_attempt).collect(),
        attempt_count,
        attempts_truncated: truncated,
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
        limits: reservation.limits,
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
        usage: attempt.usage,
        created_at: attempt.created_at,
        updated_at: attempt.updated_at,
        finished_at: attempt.finished_at,
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
        store.save_run_with_quota(&run, &reservation).unwrap();
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
            .split("pub fn list_runs_scoped")
            .nth(1)
            .expect("list_runs_scoped")
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
            .split("pub fn list_public_session_runs")
            .nth(1)
            .expect("list_public_session_runs")
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
}
