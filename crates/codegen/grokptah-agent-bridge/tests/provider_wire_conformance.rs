//! Hermetic production-client replay for versioned xAI provider fixtures.

use std::path::{Component, Path, PathBuf};

use grokptah_agent_bridge::{
    public_xai_endpoint_fingerprint, replay_xai_provider_contract_on_loopback, ArtifactReference,
    AttemptDisposition, CampaignActuals, CampaignBudgets, CampaignIdentity, CertificationCheck,
    CredentialMethodClass, PersistentAgentCapture, ProviderAttemptEvidence, ProviderDialectClass,
    ProviderIdentity, ProviderRouteClass, StreamFraming, UsageEvidence,
    PERSISTENT_AGENT_CAPTURE_SCHEMA,
};
use grokptah_test_gateway::{split_at, MockGateway, Response, Step};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const FIXTURE_SCHEMA: &str = "grokptah.provider_contract_fixture.v1";
const MAX_FIXTURE_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderContractFixture {
    schema: String,
    scenario_id: String,
    provider_route_class: String,
    dialect: String,
    synthetic: bool,
    response_artifact: String,
    expected: FixtureExpectations,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureExpectations {
    content_marker: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    terminal_marker: String,
}

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../evals/provider-contracts/xai/v1/sse-stream-001")
}

fn load_fixture() -> (ProviderContractFixture, Vec<u8>) {
    let root = dunce::canonicalize(fixture_directory()).expect("fixture directory must exist");
    let fixture: ProviderContractFixture = serde_json::from_slice(
        &std::fs::read(root.join("fixture.json")).expect("fixture manifest must be readable"),
    )
    .expect("fixture manifest must match its versioned schema");

    assert_eq!(fixture.schema, FIXTURE_SCHEMA);
    assert_eq!(fixture.scenario_id, "sse-stream-001");
    assert_eq!(fixture.provider_route_class, "grok_build_proxy");
    assert_eq!(fixture.dialect, "xai_chat_completions");
    assert!(fixture.synthetic, "only synthetic fixtures may be replayed");
    assert_eq!(
        fixture.expected.total_tokens,
        fixture
            .expected
            .prompt_tokens
            .checked_add(fixture.expected.completion_tokens)
            .expect("fixture token total must not overflow")
    );

    let relative = Path::new(&fixture.response_artifact);
    assert!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "fixture artifact must be a non-empty normalized relative path"
    );
    let artifact_path =
        dunce::canonicalize(root.join(relative)).expect("fixture response artifact must exist");
    assert!(
        artifact_path.starts_with(&root),
        "fixture artifact must remain beneath its fixture directory"
    );
    let metadata = artifact_path
        .metadata()
        .expect("fixture response metadata must be readable");
    assert!(metadata.is_file(), "fixture response must be a file");
    assert!(
        metadata.len() <= MAX_FIXTURE_ARTIFACT_BYTES,
        "fixture response exceeds replay bound"
    );
    let response = std::fs::read(&artifact_path).expect("fixture response must be readable");
    let terminal_line = format!("data: {}", fixture.expected.terminal_marker);
    assert!(
        response
            .split(|byte| *byte == b'\n')
            .any(|line| line.strip_suffix(b"\r").unwrap_or(line) == terminal_line.as_bytes()),
        "fixture response must contain its declared terminal marker"
    );
    (fixture, response)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn synthetic_xai_fixture_replays_through_the_production_provider_path() {
    let (fixture, response) = load_fixture();
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::sse_stream(split_at(
        &response,
        &[1, 17, 51, response.len() - 1],
    )))])
    .await;
    let messages = serde_json::json!([
        {"role": "system", "content": "Synthetic provider contract replay."},
        {"role": "user", "content": "Return the fixture marker."}
    ]);
    let tools = serde_json::json!([]);
    let replay = replay_xai_provider_contract_on_loopback(
        &format!("{}/v1", gateway.base_url()),
        "grok-fixture",
        messages.as_array().unwrap(),
        &tools,
    )
    .await
    .expect("production xAI provider path must accept the fixture");

    assert_eq!(replay.text, fixture.expected.content_marker);
    assert_eq!(
        replay.deltas.as_slice(),
        std::slice::from_ref(&fixture.expected.content_marker)
    );
    assert!(replay.streamed);
    assert!(replay.reasoning.is_none());
    assert!(replay.thought_deltas.is_empty());
    let usage = replay.usage.expect("fixture must expose provider usage");
    assert_eq!(usage.prompt_tokens, fixture.expected.prompt_tokens);
    assert_eq!(usage.completion_tokens, fixture.expected.completion_tokens);
    assert_eq!(usage.total_tokens, fixture.expected.total_tokens);
    assert_eq!(usage.requests, 1);

    let requests = gateway.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    assert!(request.header("content-type").is_some());
    assert!(request.header("authorization").is_some());
    assert!(request.header("x-xai-token-auth").is_some());
    assert!(request.header("x-grok-client-version").is_some());
    assert_eq!(request.header("x-grok-client-mode"), Some("interactive"));
    assert_eq!(request.header("x-grok-effort"), Some("none"));
    assert!(request.header("x-userid").is_some());
    assert!(request.header("x-teamid").is_some());

    let body = request
        .json_body()
        .expect("production provider request must be JSON");
    assert_eq!(body["model"], "grok-fixture");
    assert_eq!(body["messages"], messages);
    assert_eq!(body["tools"], tools);
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn production_provider_path_rejects_a_fixture_without_its_terminal_marker() {
    let (fixture, response) = load_fixture();
    let terminal = format!("data: {}\n", fixture.expected.terminal_marker);
    let truncated = response
        .strip_suffix(terminal.as_bytes())
        .expect("fixture terminal marker must be the final SSE event");
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::sse_stream(split_at(
        truncated,
        &[1, truncated.len() - 1],
    )))])
    .await;
    let error = replay_xai_provider_contract_on_loopback(
        &format!("{}/v1", gateway.base_url()),
        "grok-fixture",
        &[serde_json::json!({"role": "user", "content": "synthetic"})],
        &serde_json::json!([]),
    )
    .await
    .expect_err("production SSE handling must require the completion marker");
    assert!(error.to_string().contains("completion marker"));
}

#[tokio::test]
async fn provider_contract_replay_refuses_non_loopback_authority() {
    let error = replay_xai_provider_contract_on_loopback(
        "https://api.x.ai/v1",
        "grok-fixture",
        &[serde_json::json!({"role": "user", "content": "synthetic"})],
        &serde_json::json!([]),
    )
    .await
    .expect_err("test-support replay must never target remote authority");
    assert!(error.to_string().contains("loopback"));
}

#[test]
fn synthetic_xai_fixture_has_a_promotable_secret_free_capture() {
    let (fixture, response) = load_fixture();
    let capture = PersistentAgentCapture {
        schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
        campaign: CampaignIdentity {
            campaign_id: format!("opaque-{}", "9".repeat(64)),
            scenario_id: fixture.scenario_id,
            repository_commit: "b6dab133".into(),
            dirty: false,
        },
        provider: ProviderIdentity {
            route_class: ProviderRouteClass::GrokBuildProxy,
            dialect: ProviderDialectClass::XaiChatCompletions,
            credential_method: CredentialMethodClass::GrokBuildOidc,
            model_identity: "grok-fixture".into(),
            endpoint_fingerprint: public_xai_endpoint_fingerprint(
                &ProviderRouteClass::GrokBuildProxy,
            )
            .unwrap(),
        },
        budgets: CampaignBudgets {
            bound_profile: grokptah_agent_bridge::CertificationBoundProfile::Smoke,
            max_run_tokens: 20_000,
            max_total_tokens: 100_000,
            max_provider_requests: 40,
            max_continuations: 4,
            max_duration_seconds: 1_800,
            max_raw_artifact_bytes: 128 * 1024 * 1024,
            max_response_bytes_per_request: MAX_FIXTURE_ARTIFACT_BYTES,
        },
        actuals: CampaignActuals {
            total_tokens: fixture.expected.total_tokens,
            provider_requests: 1,
            continuations: 0,
            duration_seconds: 1,
            raw_artifact_bytes: response.len() as u64,
        },
        attempts: vec![ProviderAttemptEvidence {
            attempt: 1,
            method: "POST".into(),
            route_identity: "/v1/chat/completions".into(),
            present_request_headers: vec!["content-type".into(), "user-agent".into()],
            request_body: None,
            response_body: Some(ArtifactReference {
                relative_path: fixture.response_artifact,
                sha256: sha256(&response),
                bytes: response.len() as u64,
            }),
            response_status: Some(200),
            response_content_type: Some("text/event-stream".into()),
            framing: StreamFraming::Sse,
            disposition: AttemptDisposition::Success,
            usage: Some(UsageEvidence {
                prompt_tokens: fixture.expected.prompt_tokens,
                completion_tokens: fixture.expected.completion_tokens,
                total_tokens: fixture.expected.total_tokens,
                complete: true,
            }),
            latency_millis: 1,
        }],
        durable_states: vec![grokptah_agent_bridge::DurableStateEvidence {
            role: grokptah_agent_bridge::certification::DurableEvidenceRole::Primary,
            agent_id: format!("opaque-{}", "1".repeat(64)),
            agent_spec_revision: 1,
            lane_id: format!("opaque-{}", "2".repeat(64)),
            run_id: format!("opaque-{}", "3".repeat(64)),
            parent_run_id: None,
            checkpoint_id: Some(format!("opaque-{}", "4".repeat(64))),
            continuation_input_hash: None,
            continuation_context_hash: None,
            continuation_fidelity: Some("complete".into()),
            terminal_state: "completed".into(),
            stop_cause: None,
        }],
        checks: [
            "ordered_text_or_tool_deltas",
            "terminal_frame_observed",
            "authoritative_usage_present",
            "provider_observation_complete",
            "provider_attempts_bound_to_durable_run",
        ]
        .into_iter()
        .map(|name| CertificationCheck {
            name: name.into(),
            passed: true,
            detail_code: "production-provider-path-and-usage-match".into(),
        })
        .collect(),
    };
    capture
        .validate_for_xai_fixture_promotion_at(&fixture_directory())
        .unwrap();
}
