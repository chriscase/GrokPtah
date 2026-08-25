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
    public_attempt_http_status, public_error_http_status, public_text_leaks_secret,
    verify_campaign, AttemptOutcome, CampaignBundle, EvidenceKind, FakeQuotaMode,
    FakeRestrictedGateway, GatewayIdentityRecord, ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA,
    PUBLIC_SECRET_NEEDLES,
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
        Ok((identity, quota, attempt)) => {
            let status = StatusCode::from_u16(public_attempt_http_status(&attempt))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (
                status,
                Json(json!({
                    "identity": identity,
                    "quota": quota,
                    "attempt": attempt,
                })),
            )
                .into_response()
        }
        Err(error) => {
            let status = StatusCode::from_u16(public_error_http_status(&error))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(error)).into_response()
        }
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

fn public_json_is_needle_free(value: &serde_json::Value) {
    fn check_envelope(envelope: &serde_json::Value) {
        if let Some(message) = envelope["message"].as_str() {
            assert!(
                !public_text_leaks_secret(message),
                "public envelope message leaked a secret: {envelope}"
            );
        }
        if let Some(reason) = envelope["reasonCode"].as_str() {
            assert!(
                !public_text_leaks_secret(reason),
                "public envelope reason leaked a secret: {envelope}"
            );
        }
        let serialized = envelope.to_string().to_ascii_lowercase();
        for needle in PUBLIC_SECRET_NEEDLES {
            assert!(
                !serialized.contains(*needle),
                "public envelope contained needle `{needle}`: {envelope}"
            );
        }
        assert!(
            !serialized.contains("://"),
            "public envelope contained a provider URL: {envelope}"
        );
    }

    if value.get("code").is_some() && value.get("message").is_some() {
        check_envelope(value);
    }
    if value["error"].is_object() {
        check_envelope(&value["error"]);
    }
    if value["attempt"]["error"].is_object() {
        check_envelope(&value["attempt"]["error"]);
    }
}

fn campaign_bundle_from_http(
    request_id: &str,
    requested: GatewayIdentityRecord,
    first: &serde_json::Value,
    retry: &serde_json::Value,
) -> CampaignBundle {
    serde_json::from_value(json!({
        "schema": ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA,
        "requestId": request_id,
        "requested": requested,
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
    .unwrap()
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
    assert_eq!(first["quota"]["requestId"], "req-http");
    assert_eq!(first["quota"]["profileId"], first["identity"]["profileId"]);
    assert_eq!(first["quota"]["modelId"], first["identity"]["modelId"]);
    assert_eq!(
        first["quota"]["providerKind"],
        first["identity"]["providerKind"]
    );
    assert_eq!(first["quota"]["baseUrl"], first["identity"]["baseUrl"]);

    let mut bundle: grokptah_agent_bridge::CampaignBundle = serde_json::from_value(json!({
        "schema": ENTERPRISE_GATEWAY_CAMPAIGN_SCHEMA,
        "requestId": "req-http",
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
    assert!(verdict
        .remaining_live_gates
        .iter()
        .any(|gate| gate.contains("live HTTPS retry/idempotency")));
    assert!(verdict
        .remaining_live_gates
        .iter()
        .any(|gate| gate.contains("release artifact")));
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
        "requestId": "req-http-fallback",
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
async fn loopback_http_unknown_quota_and_unavailable_stay_fail_closed_and_needle_free() {
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
    let requested = down.requested_identity();
    let (base, server) = start_fake_gateway(down).await;
    let (status, first) = http_probe(&client, &base, "req-http-down", "review")
        .await
        .unwrap();
    let (retry_status, retry) = http_probe(&client, &base, "req-http-down", "review")
        .await
        .unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(first["attempt"]["outcome"], "failed");
    assert_eq!(first["attempt"]["error"]["code"], "authority_unavailable");
    assert_eq!(
        first["attempt"]["error"]["reasonCode"],
        "provider_unavailable"
    );
    assert_eq!(
        first["attempt"]["error"]["message"],
        "The requested provider is unavailable."
    );
    assert_eq!(retry["attempt"]["outcome"], "replayed");
    assert_eq!(retry["attempt"]["error"], first["attempt"]["error"]);
    public_json_is_needle_free(&first);
    public_json_is_needle_free(&retry);
    let bundle = campaign_bundle_from_http("req-http-down", requested, &first, &retry);
    let verdict = verify_campaign(&bundle);
    assert!(!verdict.qualified_for_release);
    let redaction = verdict
        .checks
        .iter()
        .find(|check| check.name == "bounded_errors_and_redaction")
        .unwrap();
    assert!(redaction.passed, "{verdict:#?}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_http_canonical_duplicate_retry_and_failure_replay() {
    let (base, server) = start_fake_gateway(FakeRestrictedGateway::restricted_loopback(
        "http://127.0.0.1:0/v1",
    ))
    .await;
    let client = reqwest::Client::new();
    let left = r#"{"task":"review","n":1}"#;
    let right = r#"{ "n": 1, "task": "review" }"#;
    let (first_status, first) = http_probe(&client, &base, "req-http-canon", left)
        .await
        .unwrap();
    let (retry_status, retry) = http_probe(&client, &base, "req-http-canon", right)
        .await
        .unwrap();
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry["attempt"]["outcome"], "replayed");
    assert_eq!(
        first["attempt"]["payloadHash"],
        retry["attempt"]["payloadHash"]
    );
    assert!(retry["attempt"]["error"].is_null());
    server.abort();

    let down =
        FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:0/v1").with_unavailable();
    let requested = down.requested_identity();
    let (base, server) = start_fake_gateway(down).await;
    let (first_status, first) = http_probe(&client, &base, "req-http-fail-replay", left)
        .await
        .unwrap();
    let (retry_status, retry) = http_probe(&client, &base, "req-http-fail-replay", left)
        .await
        .unwrap();
    assert_eq!(first_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(first["attempt"]["outcome"], "failed");
    assert_eq!(retry["attempt"]["outcome"], "replayed");
    assert_eq!(first["attempt"]["error"], retry["attempt"]["error"]);
    public_json_is_needle_free(&first);
    public_json_is_needle_free(&retry);
    let bundle = campaign_bundle_from_http("req-http-fail-replay", requested, &first, &retry);
    assert_eq!(bundle.attempts[0].outcome, AttemptOutcome::Failed);
    assert_eq!(bundle.attempts[1].outcome, AttemptOutcome::Replayed);
    assert_eq!(bundle.attempts[0].error, bundle.attempts[1].error);
    let verdict = verify_campaign(&bundle);
    assert!(!verdict.qualified_for_release);
    let retry_check = verdict
        .checks
        .iter()
        .find(|check| check.name == "idempotent_retry_receipts")
        .unwrap();
    assert!(retry_check.passed, "{verdict:#?}");
    let redaction = verdict
        .checks
        .iter()
        .find(|check| check.name == "bounded_errors_and_redaction")
        .unwrap();
    assert!(redaction.passed, "{verdict:#?}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_http_invalid_request_stays_distinct_from_unavailable() {
    let (base, server) = start_fake_gateway(FakeRestrictedGateway::restricted_loopback(
        "http://127.0.0.1:0/v1",
    ))
    .await;
    let client = reqwest::Client::new();
    let (first_status, first) = http_probe(&client, &base, "req-http-drift", "first")
        .await
        .unwrap();
    let (drift_status, drift) = http_probe(&client, &base, "req-http-drift", "second")
        .await
        .unwrap();
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first["attempt"]["outcome"], "succeeded");
    assert_eq!(drift_status, StatusCode::BAD_REQUEST);
    assert_ne!(drift_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(drift["code"], "invalid_request");
    assert_eq!(drift["reasonCode"], "invalid_request");
    assert_eq!(drift["message"], "The request is invalid.");
    public_json_is_needle_free(&drift);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_http_pending_and_uncertain_keep_status_and_uncertainty() {
    let pending =
        FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:0/v1").with_pending();
    let requested = pending.requested_identity();
    let (base, server) = start_fake_gateway(pending).await;
    let client = reqwest::Client::new();
    let (first_status, first) = http_probe(&client, &base, "req-http-pending", "review")
        .await
        .unwrap();
    let (retry_status, retry) = http_probe(&client, &base, "req-http-pending", "review")
        .await
        .unwrap();
    assert_eq!(first_status, StatusCode::ACCEPTED);
    assert_eq!(retry_status, StatusCode::ACCEPTED);
    assert_eq!(first["attempt"]["outcome"], "pending");
    assert_eq!(retry["attempt"]["outcome"], "replayed");
    assert_eq!(first["attempt"]["error"]["reasonCode"], "pending");
    assert_eq!(retry["attempt"]["error"], first["attempt"]["error"]);
    public_json_is_needle_free(&first);
    public_json_is_needle_free(&retry);
    let bundle = campaign_bundle_from_http("req-http-pending", requested, &first, &retry);
    let verdict = verify_campaign(&bundle);
    assert!(!verdict.contract_passed);
    assert!(!verdict.qualified_for_release);
    let retry_check = verdict
        .checks
        .iter()
        .find(|check| check.name == "idempotent_retry_receipts")
        .unwrap();
    assert!(!retry_check.passed);
    assert!(retry_check.detail.contains("pending"));
    server.abort();

    let uncertain =
        FakeRestrictedGateway::restricted_loopback("http://127.0.0.1:0/v1").with_uncertain();
    let requested = uncertain.requested_identity();
    let (base, server) = start_fake_gateway(uncertain).await;
    let (first_status, first) = http_probe(&client, &base, "req-http-uncertain", "review")
        .await
        .unwrap();
    let (retry_status, retry) = http_probe(&client, &base, "req-http-uncertain", "review")
        .await
        .unwrap();
    assert_eq!(first_status, StatusCode::CONFLICT);
    assert_eq!(retry_status, StatusCode::CONFLICT);
    assert_ne!(first_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(first["attempt"]["outcome"], "uncertain");
    assert_eq!(retry["attempt"]["outcome"], "replayed");
    assert_eq!(first["attempt"]["error"]["reasonCode"], "uncertain");
    assert_eq!(retry["attempt"]["error"], first["attempt"]["error"]);
    public_json_is_needle_free(&first);
    public_json_is_needle_free(&retry);
    let bundle = campaign_bundle_from_http("req-http-uncertain", requested, &first, &retry);
    let verdict = verify_campaign(&bundle);
    assert!(!verdict.contract_passed);
    let retry_check = verdict
        .checks
        .iter()
        .find(|check| check.name == "idempotent_retry_receipts")
        .unwrap();
    assert!(!retry_check.passed);
    assert!(retry_check.detail.contains("uncertain"));
    server.abort();
}

#[test]
fn offline_fixture_kind_is_never_live_proof() {
    assert_ne!(EvidenceKind::OfflineFixture, EvidenceKind::LiveCampaign);
    assert_ne!(EvidenceKind::Absent, EvidenceKind::LiveCampaign);
}
