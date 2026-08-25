//! Offline fake-gateway campaign over loopback HTTP.
//!
//! These tests exercise the enterprise evidence contract without credentials
//! or a live company/Cursor route. A passing fixture is not live proof and
//! must not qualify a release.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use grokptah_agent_bridge::{
    verify_campaign, AttemptOutcome, EvidenceKind, FakeQuotaMode, FakeRestrictedGateway,
    ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA,
};
use serde_json::json;

#[derive(Clone)]
struct GatewayState {
    gateway: Arc<FakeRestrictedGateway>,
}

async fn probe_handler(
    State(state): State<GatewayState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let request_id = body["request_id"].as_str().unwrap_or_default();
    let payload = body["payload"].as_str().unwrap_or_default();
    match state.gateway.probe(request_id, payload) {
        Ok((identity, quota, attempt)) => (
            StatusCode::OK,
            Json(json!({
                "identity": identity,
                "quota": quota,
                "attempt": attempt,
            })),
        )
            .into_response(),
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, Json(error)).into_response(),
    }
}

async fn start_fake_gateway(
    gateway: FakeRestrictedGateway,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/campaign/probe", post(probe_handler))
        .with_state(GatewayState {
            gateway: Arc::new(gateway),
        });
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), task)
}

async fn http_probe(
    client: &reqwest::Client,
    base: &str,
    request_id: &str,
    payload: &str,
) -> Result<(StatusCode, serde_json::Value), reqwest::Error> {
    let response = client
        .post(format!("{base}/campaign/probe"))
        .json(&json!({ "request_id": request_id, "payload": payload }))
        .send()
        .await?;
    let status = response.status();
    let value = response.json().await?;
    Ok((status, value))
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_http_fixture_records_identity_quota_and_replay_but_refuses_release() {
    let (base, server) = start_fake_gateway(FakeRestrictedGateway::restricted_loopback(
        "http://127.0.0.1:0/v1",
    ))
    .await;
    let client = reqwest::Client::new();
    let (first_status, first) = http_probe(&client, &base, "req-http", "restricted review")
        .await
        .unwrap();
    let (retry_status, retry) = http_probe(&client, &base, "req-http", "restricted review")
        .await
        .unwrap();
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry["attempt"]["outcome"], "replayed");
    assert_eq!(first["identity"]["class"], "restricted_company");
    assert_eq!(first["identity"]["profileId"], "corp-restricted");
    assert_eq!(first["quota"]["truth"]["kind"], "provider_receipt");
    assert_eq!(first["quota"]["truth"]["source"], "provider");
    assert_eq!(first["quota"]["truth"]["evidenceKind"], "offline_fixture");

    let mut bundle: grokptah_agent_bridge::CampaignBundle = serde_json::from_value(json!({
        "schema": ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA,
        "requested": {
            "profileId": "corp-restricted",
            "baseUrl": "http://127.0.0.1:9/v1",
            "modelId": "company-code-small",
            "tenant": "acme-tenant",
            "class": "restricted_company",
            "providerKind": "open_ai_compatible"
        },
        "observed": first["identity"],
        "quota": first["quota"],
        "attempts": [first["attempt"], retry["attempt"]],
        "cursorAccount": { "provider": "cursor_cloud", "kind": "absent" },
        "promotion": {
            "liveGateway": "absent",
            "liveQuota": "absent",
            "liveCursorAccount": "absent"
        }
    }))
    .unwrap();
    bundle.requested.base_url = bundle.observed.base_url.clone();
    let verdict = verify_campaign(&bundle);
    assert!(verdict.contract_passed, "{verdict:#?}");
    assert!(!verdict.qualified_for_release);
    assert_eq!(bundle.attempts[1].outcome, AttemptOutcome::Replayed);
    assert!(verdict
        .remaining_live_gates
        .iter()
        .any(|gate| gate.contains("live restricted-company gateway")));
    assert!(verdict
        .remaining_live_gates
        .iter()
        .any(|gate| gate.contains("live provider quota")));
    assert!(verdict
        .remaining_live_gates
        .iter()
        .any(|gate| gate.contains("live Cursor-account")));
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_http_fallback_to_frontier_is_detected() {
    let gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:0/v1")
        .with_silent_frontier_fallback();
    let (base, server) = start_fake_gateway(gateway).await;
    let client = reqwest::Client::new();
    let (status, first) = http_probe(&client, &base, "req-http-fallback", "review")
        .await
        .unwrap();
    let (_, retry) = http_probe(&client, &base, "req-http-fallback", "review")
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["identity"]["class"], "frontier");
    assert_eq!(first["identity"]["profileId"], "xai");
    let bundle: grokptah_agent_bridge::CampaignBundle = serde_json::from_value(json!({
        "schema": ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA,
        "requested": {
            "profileId": "corp-restricted",
            "baseUrl": "http://127.0.0.1:9/v1",
            "modelId": "company-code-small",
            "tenant": "acme-tenant",
            "class": "restricted_company",
            "providerKind": "open_ai_compatible"
        },
        "observed": first["identity"],
        "quota": first["quota"],
        "attempts": [first["attempt"], retry["attempt"]],
        "cursorAccount": { "provider": "cursor_cloud", "kind": "absent" },
        "promotion": {
            "liveGateway": "absent",
            "liveQuota": "absent",
            "liveCursorAccount": "absent"
        }
    }))
    .unwrap();
    let verdict = verify_campaign(&bundle);
    assert!(!verdict.contract_passed);
    assert!(!verdict.qualified_for_release);
    let fallback = verdict
        .checks
        .iter()
        .find(|check| check.name == "no_silent_frontier_fallback")
        .unwrap();
    assert!(!fallback.passed);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_http_unknown_quota_and_unavailable_stay_fail_closed_and_redacted() {
    let quota_gateway = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:0/v1")
        .with_quota_mode(FakeQuotaMode::Unknown);
    let (base, server) = start_fake_gateway(quota_gateway).await;
    let client = reqwest::Client::new();
    let (_, first) = http_probe(&client, &base, "req-http-quota", "review")
        .await
        .unwrap();
    assert_eq!(first["quota"]["truth"]["kind"], "unknown");
    server.abort();

    let secret = "sk-live-secret-value";
    let down = FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:0/v1")
        .with_unavailable()
        .with_leaked_secret(secret);
    let (base, server) = start_fake_gateway(down).await;
    let (status, error) = http_probe(&client, &base, "req-http-down", "review")
        .await
        .unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["code"], "authority_unavailable");
    let serialized = error.to_string();
    assert!(!serialized.contains(secret));
    assert!(!serialized.contains("evil.example"));
    assert!(!serialized.contains("api_key=sk-"));
    assert_eq!(error["reasonCode"], "provider_unavailable");
    server.abort();
}

#[test]
fn offline_fixture_kind_is_never_live_proof() {
    assert_ne!(EvidenceKind::OfflineFixture, EvidenceKind::LiveCampaign);
    assert_ne!(EvidenceKind::Absent, EvidenceKind::LiveCampaign);
}
