//! Public Run projection and shared Execution admission at the hosted MCP surface.

mod common;

use grokptah_agent_bridge::orchestration::{
    hash_payload, public_run_contains_forbidden_fields, ProviderRouteSnapshot, RunAggregates,
    RunBounds, RunPurpose, RunRecord, RunState, PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
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

fn leaky_route() -> ProviderRouteSnapshot {
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
            source: CapabilitySource::Declared,
            ..ModelCapabilities::default()
        },
        deadline_class: ProviderDeadlineClass::Standard,
        effort: EffortLevel::Medium,
        qualification_record_id: None,
        quota_class: None,
        quota_reservation_id: None,
        snapshot_hash: "pending".into(),
    };
    route.endpoint_fingerprint = hash_payload(&json!({
        "kind": route.kind,
        "dialect": route.dialect,
        "baseUrl": route.base_url,
    }));
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
    });
    route.snapshot_hash = hash_payload(&material);
    route
        .validate()
        .expect("reconstructed leaky route must validate");
    route
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
    for sentinel in [
        route.base_url.as_str(),
        route.credential_ref.as_str(),
        route.credential_fingerprint.as_str(),
        route.endpoint_fingerprint.as_str(),
        BASE_URL_SENTINEL,
        CREDENTIAL_REF_SENTINEL,
        CREDENTIAL_FP_SENTINEL,
    ] {
        assert!(
            !encoded.contains(sentinel),
            "hosted payload leaked {sentinel}: {encoded}"
        );
    }
    assert!(!public_run_contains_forbidden_fields(payload));
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
        bounds: RunBounds::default(),
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
    host.ensure_orchestration_store()
        .unwrap()
        .save_run(&run)
        .unwrap();

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
    assert_payload_hides_route(&get_run.structured, &route);
    assert_payload_hides_route(&list_runs.structured, &route);
    assert_payload_hides_route(&list_runs.structured["runs"][0], &route);
    assert_payload_hides_route(&progress.structured, &route);
    let persisted = serde_json::to_value(
        host.ensure_orchestration_store()
            .unwrap()
            .load_run(&run.run_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(persisted.get("providerRoute").is_some());
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
        .list_session_runs(session_id)
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
