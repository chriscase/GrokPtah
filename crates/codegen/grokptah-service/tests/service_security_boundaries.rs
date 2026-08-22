//! Public Run projection and shared Execution admission at the hosted MCP surface.

mod common;

use grokptah_agent_bridge::orchestration::{
    hash_payload, public_provider_route_keys_are_allowlisted, public_run_contains_forbidden_fields,
    IdempotencyClaim, ProviderRouteSnapshot, QuotaClass, QuotaLimits, QuotaReservation,
    RunAggregates, RunBounds, RunPurpose, RunRecord, RunState,
    PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
};
use grokptah_agent_bridge::{
    model_selection_key, CapabilitySource, EffortLevel, McpRemoteError, ModelCapabilities,
    ProviderDeadlineClass, ProviderDialect, ProviderKind,
};
use serde_json::{json, Value};

use common::{create_build_session, mcp_client, start_isolated, ServiceEnv};

const BASE_URL_SENTINEL: &str = "http://127.0.0.1:35201/leak-base-url-sentinel-pr352/v1";
const CREDENTIAL_REF_SENTINEL: &str = "keychain:provider/leak-cred-ref-sentinel-pr352";
const CREDENTIAL_FP_SENTINEL: &str = "v1-sha256:leak-cred-fp-sentinel-pr352";
const QUOTA_RESERVATION_SENTINEL: &str = "quota-leak-reservation-sentinel-pr352";
const QUALIFICATION_SCHEMA: &str = "grokptah.provider-qualification.v1";

fn leaky_route() -> ProviderRouteSnapshot {
    leaky_route_with_quota(QUOTA_RESERVATION_SENTINEL)
}

fn leaky_route_with_quota(reservation_id: &str) -> ProviderRouteSnapshot {
    let mut route = ProviderRouteSnapshot {
        schema_version: PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
        provider_id: "env-grokptah".into(),
        model_id: "leak-model".into(),
        wire_model_id: "leak-model".into(),
        selection_key: model_selection_key("env-grokptah", "leak-model"),
        kind: ProviderKind::OpenAiCompatible,
        dialect: ProviderDialect::OpenAiChatCompletions,
        base_url: BASE_URL_SENTINEL.into(),
        endpoint_fingerprint: "pending".into(),
        credential_ref: CREDENTIAL_REF_SENTINEL.into(),
        credential_fingerprint: CREDENTIAL_FP_SENTINEL.into(),
        capabilities: ModelCapabilities {
            chat: true,
            tools: true,
            stream: true,
            source: CapabilitySource::Measured,
            qualification_schema: Some(QUALIFICATION_SCHEMA.into()),
            ..ModelCapabilities::default()
        },
        deadline_class: ProviderDeadlineClass::Standard,
        effort: EffortLevel::Medium,
        qualification_record_id: None,
        quota_class: Some(QuotaClass::CodingExecution),
        quota_reservation_id: Some(reservation_id.into()),
        snapshot_hash: "pending".into(),
    };
    route.endpoint_fingerprint = hash_payload(&json!({
        "kind": route.kind,
        "dialect": route.dialect,
        "baseUrl": route.base_url,
    }));
    route.qualification_record_id = Some(hash_payload(&json!({
        "providerId": route.provider_id,
        "modelId": route.model_id,
        "wireModelId": route.wire_model_id,
        "endpointFingerprint": route.endpoint_fingerprint,
        "credentialFingerprint": route.credential_fingerprint,
        "qualificationSchema": route.capabilities.qualification_schema,
    })));
    let material = json!({
        "schemaVersion": route.schema_version,
        "providerId": route.provider_id,
        "modelId": route.model_id,
        "wireModelId": route.wire_model_id,
        "selectionKey": route.selection_key,
        "kind": route.kind,
        "dialect": route.dialect,
        "baseUrl": route.base_url,
        "endpointFingerprint": route.endpoint_fingerprint,
        "credentialRef": route.credential_ref,
        "credentialFingerprint": route.credential_fingerprint,
        "capabilities": route.capabilities,
        "deadlineClass": route.deadline_class,
        "effort": route.effort,
        "qualificationRecordId": route.qualification_record_id,
        "quotaClass": route.quota_class,
        "quotaReservationId": route.quota_reservation_id,
    });
    route.snapshot_hash = hash_payload(&material);
    route
        .validate()
        .expect("reconstructed leaky route must validate");
    route
}

fn mcp_text_value(raw: &Value) -> Value {
    let text = raw
        .get("content")
        .and_then(|content| content.get(0))
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .expect("MCP tool result must include content[0].text");
    serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }))
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
        BASE_URL_SENTINEL,
        CREDENTIAL_REF_SENTINEL,
        CREDENTIAL_FP_SENTINEL,
    ] {
        if sentinel.is_empty() {
            continue;
        }
        assert!(
            !encoded.contains(sentinel),
            "hosted payload leaked {sentinel}: {encoded}"
        );
    }
    assert!(!public_run_contains_forbidden_fields(payload));
    if let Some(route_json) = payload
        .get("providerExecution")
        .and_then(|value| value.get("route"))
        .or_else(|| {
            payload
                .get("runs")
                .and_then(|runs| runs.get(0))
                .and_then(|run| run.get("providerExecution"))
                .and_then(|value| value.get("route"))
        })
    {
        assert!(route_json.get("quotaReservationId").is_none());
        assert!(route_json.get("selectionKey").is_none());
        assert!(route_json.get("qualificationRecordId").is_none());
        assert!(
            public_provider_route_keys_are_allowlisted(route_json),
            "providerExecution.route keys must be exact-allowlisted: {route_json}"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn hosted_list_and_get_omit_frozen_provider_route() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 2).await;
    let host = handle.host();
    let mut client = mcp_client(handle.addr).await;
    let session_id = create_build_session(&mut client, &workspace, "Public run projection").await;
    let route = leaky_route();
    let now = chrono::Utc::now();
    let run = RunRecord {
        run_id: "hosted-leaky-run".into(),
        session_id,
        workspace: workspace.display().to_string(),
        request_id: "hosted-leaky-req".into(),
        client_id: Some("mcp".into()),
        state: RunState::Running,
        purpose: RunPurpose::Execution,
        provider_route: Some(route.clone()),
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
    };
    let store = host.ensure_orchestration_store().unwrap();
    let reservation =
        QuotaReservation::for_run(&run, "primary", QuotaLimits::default(), now).unwrap();
    store.save_run_with_quota(&run, &reservation).unwrap();

    let get_run = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run.run_id,
            }),
        )
        .await
        .unwrap();
    let list_runs = client
        .call_tool(
            "ptah_list_runs",
            json!({
                "session_id": session_id,
                "workspace": workspace,
            }),
        )
        .await
        .unwrap();
    let progress = client
        .call_tool(
            "ptah_get_progress",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run.run_id,
            }),
        )
        .await
        .unwrap();
    assert!(!get_run.is_error, "{:?}", get_run.raw);
    assert!(!list_runs.is_error, "{:?}", list_runs.raw);
    assert!(!progress.is_error, "{:?}", progress.raw);
    for payload in [
        &get_run.structured,
        &mcp_text_value(&get_run.raw),
        &list_runs.structured,
        &list_runs.structured["runs"][0],
        &mcp_text_value(&list_runs.raw),
        &progress.structured,
        &mcp_text_value(&progress.raw),
    ] {
        assert_payload_hides_route(payload, &route);
    }
    let persisted = serde_json::to_value(store.load_run(&run.run_id).unwrap().unwrap()).unwrap();
    assert!(persisted.get("providerRoute").is_some());

    let promote_request = "hosted-promote-replay";
    let approval_id = "hosted-approval-replay";
    let promote_hash = hash_payload(&json!({
        "sessionId": session_id,
        "workspace": workspace.display().to_string(),
        "runId": run.run_id,
        "approvalId": approval_id,
    }));
    assert!(matches!(
        store
            .claim_idempotency("ptah_promote_run", promote_request, &promote_hash)
            .unwrap(),
        IdempotencyClaim::Perform
    ));
    store
        .complete_idempotency(
            "ptah_promote_run",
            promote_request,
            &promote_hash,
            Some(run.run_id.clone()),
            persisted.clone(),
        )
        .unwrap();
    let promote = client
        .call_tool(
            "ptah_promote_run",
            json!({
                "request_id": promote_request,
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run.run_id,
                "approval_id": approval_id,
            }),
        )
        .await
        .unwrap();
    assert!(!promote.is_error, "{:?}", promote.raw);
    assert_payload_hides_route(&promote.structured, &route);
    assert_payload_hides_route(&mcp_text_value(&promote.raw), &route);

    let discard_route = leaky_route_with_quota("quota-leak-discard-sentinel-pr352");
    let mut discarded = run.clone();
    discarded.run_id = "hosted-leaky-discard".into();
    discarded.request_id = "hosted-leaky-discard-req".into();
    discarded.state = RunState::Completed;
    discarded.provider_route = Some(discard_route.clone());
    let discard_reservation =
        QuotaReservation::for_run(&discarded, "primary", QuotaLimits::default(), now).unwrap();
    store
        .save_run_with_quota(&discarded, &discard_reservation)
        .unwrap();
    let discard_request = "hosted-discard-replay";
    let discard_hash = hash_payload(&json!({
        "sessionId": session_id,
        "workspace": workspace.display().to_string(),
        "runId": discarded.run_id,
    }));
    assert!(matches!(
        store
            .claim_idempotency("ptah_discard_run", discard_request, &discard_hash)
            .unwrap(),
        IdempotencyClaim::Perform
    ));
    store
        .complete_idempotency(
            "ptah_discard_run",
            discard_request,
            &discard_hash,
            Some(discarded.run_id.clone()),
            serde_json::to_value(&discarded).unwrap(),
        )
        .unwrap();
    let discard = client
        .call_tool(
            "ptah_discard_run",
            json!({
                "request_id": discard_request,
                "session_id": session_id,
                "workspace": workspace,
                "run_id": discarded.run_id,
            }),
        )
        .await
        .unwrap();
    assert!(!discard.is_error, "{:?}", discard.raw);
    assert_payload_hides_route(&discard.structured, &discard_route);
    assert_payload_hides_route(&mcp_text_value(&discard.raw), &discard_route);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn hosted_execution_agrees_on_requalify_after_measured_xai_history() {
    let env = ServiceEnv::new();
    let qualifications = env._home.path().join("provider-qualifications.json");
    std::fs::write(
        &qualifications,
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "qualifications": [{
                "provider_id": "xai",
                "model_id": "grok-route",
                "base_url": "https://api.x.ai/v1",
                "wire_model_id": "grok-route",
                "credential_ref": "managed:xai:api-key",
                "credential_fingerprint": "v1-sha256:hosted-measured-history",
                "capabilities": {
                    "chat": true,
                    "tools": true,
                    "stream": true,
                    "source": "measured",
                    "qualification_schema": "grokptah.provider-qualification.v1"
                }
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let _env = EnvRestore::capture(["GROKPTAH_AGENT_OFFLINE", "XAI_API_BASE", "XAI_API_KEY"]);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("XAI_API_BASE", "http://127.0.0.1:1/v1");
    std::env::set_var("XAI_API_KEY", "synthetic-hosted-p2-key");

    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 2).await;
    handle.host().set_model("grok-route".into());
    let mut client = mcp_client(handle.addr).await;
    let session_id =
        create_build_session(&mut client, &workspace, "Requalify hosted Execution").await;
    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "hosted-p2-requalify",
                "session_id": session_id,
                "workspace": workspace,
                "prompt": "must not dispatch",
            }),
        )
        .await;
    let error = submitted.expect_err("hosted Execution must refuse requalification");
    let remote = error
        .downcast_ref::<McpRemoteError>()
        .expect("typed MCP error");
    assert_eq!(remote.data_code(), Some("conflict"));
    assert!(handle
        .host()
        .list_public_session_runs(session_id)
        .unwrap()
        .is_empty());
    let store = handle.host().ensure_orchestration_store().unwrap();
    assert!(store.list_provider_attempts().unwrap().is_empty());
    assert!(store.list_quota_reservations().unwrap().is_empty());
}

struct EnvRestore {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvRestore {
    fn capture(keys: [&'static str; 3]) -> Self {
        Self {
            previous: keys
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
        }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, previous) in self.previous.drain(..).rev() {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
