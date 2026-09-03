//! Structural and fake-transport gates for sampler provider-send admission.
//!
//! No live provider calls. The mock server is deterministic and local.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use xai_grok_sampler::{SamplerConfig, SamplingClient};
use xai_grok_sampling_types::{
    ContentPart, ConversationItem, ConversationRequest, SamplingError, UserItem,
};
use xai_host_authority::{OperatorSendHost, UncertainReason};

fn test_config(base_url: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some("test-key".to_string()),
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        ..SamplerConfig::default()
    }
}

fn initialize_isolated_authority() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let root = tempfile::tempdir().expect("authority test root");
        let root = Box::leak(Box::new(root));
        xai_host_authority::install_operator_send_root(root.path());
    });
}

fn chat_request() -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from("hi"),
            }],
            ..Default::default()
        })],
        ..Default::default()
    }
}

async fn spawn_handler<H, Fut>(handler: H) -> (String, Arc<AtomicUsize>)
where
    H: Fn(Arc<AtomicUsize>, axum::body::Bytes) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = axum::response::Response> + Send + 'static,
{
    let hits = Arc::new(AtomicUsize::new(0));
    let state = hits.clone();
    let app = Router::new()
        .route(
            "/chat/completions",
            post(
                move |State(hits): State<Arc<AtomicUsize>>, body: axum::body::Bytes| {
                    let handler = handler.clone();
                    async move { handler(hits, body).await }
                },
            ),
        )
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), hits)
}

#[test]
fn sampler_client_has_no_raw_send_escape_hatch() {
    let source = include_str!("../src/client.rs");
    assert!(
        !source.contains(".send().await"),
        "sampler client must not raw-send after admission"
    );
    assert!(
        !source.contains("self.http.execute"),
        "sampler client must not execute outside provider_admission"
    );
    assert!(
        source.contains("crate::provider_admission::send_admitted"),
        "sampler client must dispatch through send_admitted"
    );
    let admission = include_str!("../src/provider_admission.rs");
    let admit = admission
        .find(".admit(")
        .expect("admission helper must call host admit");
    let execute = admission
        .find("client.execute(request)")
        .expect("admission helper must perform one client execute");
    assert!(admit < execute, "admit must precede client execute");
    assert_eq!(admission.matches("client.execute(request)").count(), 1);
}

#[tokio::test]
async fn chat_completion_hits_fake_transport_once_after_admission() {
    initialize_isolated_authority();
    let (base, hits) = spawn_handler(|hits, _body| async move {
        hits.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::OK,
            r#"{"id":"x","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
        )
            .into_response()
    })
    .await;
    let client = SamplingClient::new(test_config(&base)).unwrap();
    let result = client.conversation(chat_request()).await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn possible_write_5xx_is_not_resent_by_the_client() {
    initialize_isolated_authority();
    let (base, hits) = spawn_handler(|hits, _body| async move {
        hits.fetch_add(1, Ordering::SeqCst);
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })
    .await;
    let client = SamplingClient::new(test_config(&base)).unwrap();
    let err = client.conversation(chat_request()).await.unwrap_err();
    assert!(!err.is_proven_not_sent());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let _ = client.conversation(chat_request()).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "same body after possible-write 5xx must not be auto-resent"
    );
}

#[tokio::test]
async fn duplicate_in_flight_send_does_not_dispatch_a_second_time() {
    initialize_isolated_authority();
    let release = Arc::new(Notify::new());
    let received = Arc::new(Notify::new());
    let (base, hits) = spawn_handler({
        let release = Arc::clone(&release);
        let received = Arc::clone(&received);
        move |hits, _body| {
            let release = Arc::clone(&release);
            let received = Arc::clone(&received);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                received.notify_one();
                release.notified().await;
                StatusCode::OK.into_response()
            }
        }
    })
    .await;
    let client = SamplingClient::new(test_config(&base)).unwrap();
    let first = tokio::spawn({
        let client = client.clone();
        async move { client.conversation(chat_request()).await }
    });
    tokio::time::timeout(Duration::from_secs(1), received.notified())
        .await
        .expect("first request should reach fake transport");
    let second = client.conversation(chat_request()).await;
    match second {
        Err(SamplingError::StreamError { error_type, .. }) => {
            assert_eq!(error_type, "provider_send_denied");
        }
        other => panic!("expected denied duplicate, got {other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    release.notify_waiters();
    let _ = first.await.unwrap();
}

#[tokio::test]
async fn dropping_a_hanging_send_is_uncertain_and_blocks_resend() {
    initialize_isolated_authority();
    let received = Arc::new(Notify::new());
    let (base, hits) = spawn_handler({
        let received = Arc::clone(&received);
        move |hits, _body| {
            let received = Arc::clone(&received);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                received.notify_one();
                std::future::pending::<axum::response::Response>().await
            }
        }
    })
    .await;
    let client = SamplingClient::new(test_config(&base)).unwrap();
    let mut send = Box::pin(client.conversation(chat_request()));
    tokio::select! {
        _ = &mut send => panic!("hanging send completed"),
        result = tokio::time::timeout(Duration::from_secs(1), received.notified()) => {
            result.expect("hanging request should reach fake transport");
        }
    }
    drop(send);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let retry = client.conversation(chat_request()).await;
    assert!(matches!(
        retry,
        Err(SamplingError::StreamError { error_type, .. })
            if error_type == "provider_send_denied"
    ));
}

#[tokio::test]
async fn conversation_stream_tui_acp_path_is_admitted() {
    initialize_isolated_authority();
    let (base, hits) = spawn_handler(|hits, _body| async move {
        hits.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::OK,
            "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
        )
            .into_response()
    })
    .await;
    let client = SamplingClient::new(test_config(&base)).unwrap();
    let result = client.conversation_stream(chat_request()).await;
    assert!(result.is_ok(), "stream request should be admitted");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn stale_generation_and_wrong_principal_are_covered_by_host_authority_caller_tests() {
    // Exact generation/principal fencing is proven in
    // xai-host-authority `operator_send_admission` without HTTP.
    let _ = std::any::type_name::<OperatorSendHost>();
    let _ = UncertainReason::CancelledAfterPossibleWrite;
}
