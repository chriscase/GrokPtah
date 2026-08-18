//! Hermetic smoke coverage for the certification contract and scripted
//! provider gateway. Production-client protocol cases are added in the
//! observation-seam phase; this test keeps the fixture and gateway wired into
//! the bridge workspace now.

use grokptah_agent_bridge::{
    ArtifactReference, AttemptDisposition, CampaignBudgets, CampaignIdentity, CertificationCheck,
    CredentialMethodClass, PersistentAgentCapture, ProviderAttemptEvidence, ProviderDialectClass,
    ProviderIdentity, ProviderRouteClass, StreamFraming, UsageEvidence,
    PERSISTENT_AGENT_CAPTURE_SCHEMA,
};
use grokptah_test_gateway::{split_at, MockGateway, Response, Step};
use sha2::{Digest, Sha256};

const RESPONSE: &[u8] =
    include_bytes!("../../../../evals/provider-contracts/xai/v1/sse-stream-001/response.sse");

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn synthetic_xai_sse_fixture_replays_with_arbitrary_fragmentation() {
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::sse_stream(split_at(
        RESPONSE,
        &[1, 17, 51, RESPONSE.len() - 1],
    )))])
    .await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", gateway.base_url()))
        .header("authorization", "Bearer synthetic-never-log")
        .json(&serde_json::json!({
            "model": "grok-fixture",
            "messages": [{"role": "user", "content": "synthetic fixture"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), RESPONSE);

    let requests = gateway.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/chat/completions");
    let debug = format!("{:?}", requests[0]);
    assert!(!debug.contains("synthetic-never-log"));
}

#[test]
fn synthetic_xai_fixture_has_a_promotable_secret_free_capture() {
    let capture = PersistentAgentCapture {
        schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
        campaign: CampaignIdentity {
            scenario_id: "sse-stream-001".into(),
            repository_commit: "b6dab133".into(),
            dirty: false,
        },
        provider: ProviderIdentity {
            route_class: ProviderRouteClass::GrokBuildProxy,
            dialect: ProviderDialectClass::XaiChatCompletions,
            credential_method: CredentialMethodClass::GrokBuildOidc,
            model_identity: "grok-fixture".into(),
            endpoint_fingerprint: "a".repeat(64),
        },
        budgets: CampaignBudgets {
            max_total_tokens: 100_000,
            max_provider_requests: 40,
            max_continuations: 4,
            max_duration_seconds: 1_800,
            max_raw_artifact_bytes: 128 * 1024 * 1024,
            max_response_bytes_per_request: 8 * 1024 * 1024,
        },
        attempts: vec![ProviderAttemptEvidence {
            attempt: 1,
            method: "POST".into(),
            route_identity: "/v1/chat/completions".into(),
            present_request_headers: vec!["content-type".into(), "user-agent".into()],
            request_body: None,
            response_body: Some(ArtifactReference {
                relative_path: "sse-stream-001/response.sse".into(),
                sha256: sha256(RESPONSE),
                bytes: RESPONSE.len() as u64,
            }),
            response_status: Some(200),
            response_content_type: Some("text/event-stream".into()),
            framing: StreamFraming::Sse,
            disposition: AttemptDisposition::Success,
            usage: Some(UsageEvidence {
                prompt_tokens: 7,
                completion_tokens: 3,
                total_tokens: 10,
                complete: true,
            }),
            latency_millis: 1,
        }],
        durable_states: Vec::new(),
        checks: vec![CertificationCheck {
            name: "fragmented-sse-reassembled".into(),
            passed: true,
            detail_code: "bytes-and-usage-match".into(),
        }],
    };
    capture.validate_for_xai_fixture_promotion().unwrap();
}
