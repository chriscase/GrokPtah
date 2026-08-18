use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use grokptah_test_gateway::{split_at, Body, Frame, MockGateway, Response, Step};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test]
async fn ordered_steps_preserve_request_and_response_order() {
    let gateway = MockGateway::start_ordered(vec![
        Step::respond(Response::status_only(429).with_header("retry-after", "0")),
        Step::respond(Response::json_ok(&serde_json::json!({"ok": true}))),
    ])
    .await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{}/v1/chat/completions", gateway.base_url()))
        .header("authorization", "Bearer synthetic-test-token")
        .json(&serde_json::json!({"model": "grok-test"}))
        .send()
        .await
        .expect("first request");
    assert_eq!(first.status(), 429);
    assert_eq!(first.headers()["retry-after"], "0");

    let second = client
        .get(format!("{}/v1/models", gateway.base_url()))
        .send()
        .await
        .expect("second request");
    assert_eq!(second.status(), 200);
    assert_eq!(
        second.json::<serde_json::Value>().await.unwrap()["ok"],
        true
    );

    let requests = gateway.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].seq, 0);
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer synthetic-test-token")
    );
    assert_eq!(requests[0].json_body().unwrap()["model"], "grok-test");
    assert_eq!(requests[1].seq, 1);
    assert_eq!(requests[1].path, "/v1/models");
}

#[tokio::test]
async fn routed_steps_allow_a_fast_request_to_pass_a_delayed_one() {
    let hits = Arc::new(AtomicUsize::new(0));
    let route_hits = Arc::clone(&hits);
    let gateway = MockGateway::start_routed(move |request| {
        route_hits.fetch_add(1, Ordering::SeqCst);
        if request.path == "/slow" {
            Step::respond(Response::status_only(200).with_header_delay(Duration::from_millis(50)))
        } else {
            Step::respond(Response::status_only(204).with_header("x-route", "fast"))
        }
    })
    .await;
    let client = reqwest::Client::new();
    let slow = {
        let client = client.clone();
        let url = format!("{}/slow", gateway.base_url());
        tokio::spawn(async move { client.get(url).send().await })
    };
    tokio::time::sleep(Duration::from_millis(5)).await;

    let fast = client
        .get(format!("{}/fast", gateway.base_url()))
        .send()
        .await
        .expect("fast request");
    assert_eq!(fast.status(), 204);
    assert_eq!(fast.headers()["x-route"], "fast");
    assert_eq!(slow.await.unwrap().unwrap().status(), 200);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn routed_requests_are_reported_in_acceptance_order() {
    let gateway = MockGateway::start_routed(|_| Step::respond(Response::status_only(204))).await;
    let address = gateway.base_url().trim_start_matches("http://");

    let mut first = TcpStream::connect(address).await.expect("first connection");
    wait_until(|| gateway.accepted_count() == 1).await;
    let mut second = TcpStream::connect(address)
        .await
        .expect("second connection");
    wait_until(|| gateway.accepted_count() == 2).await;

    second
        .write_all(b"GET /second HTTP/1.1\r\nhost: localhost\r\n\r\n")
        .await
        .expect("second request");
    wait_until(|| gateway.request_count() == 1).await;
    first
        .write_all(b"GET /first HTTP/1.1\r\nhost: localhost\r\n\r\n")
        .await
        .expect("first request");
    wait_until(|| gateway.request_count() == 2).await;

    let requests = gateway.requests();
    assert_eq!(requests[0].seq, 0);
    assert_eq!(requests[0].path, "/first");
    assert_eq!(requests[1].seq, 1);
    assert_eq!(requests[1].path, "/second");
}

#[tokio::test]
async fn dropping_routed_gateway_cancels_stalled_connection_tasks() {
    let gateway = MockGateway::start_routed(|_| Step::stall()).await;
    let address = gateway.base_url().trim_start_matches("http://");
    let mut connection = TcpStream::connect(address).await.expect("connection");
    connection
        .write_all(b"GET /stall HTTP/1.1\r\nhost: localhost\r\n\r\n")
        .await
        .expect("request");
    wait_until(|| gateway.request_count() == 1).await;

    drop(gateway);

    let mut byte = [0_u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(1), connection.read(&mut byte))
        .await
        .expect("stalled child should be cancelled when gateway drops");
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "connection unexpectedly produced response data: {closed:?}"
    );
}

#[tokio::test]
async fn arbitrary_stream_fragments_and_delays_reassemble_exactly() {
    let payload = "data: {\"delta\":\"a💡b\"}\n\ndata: [DONE]\n\n".as_bytes();
    let mut frames = split_at(payload, &[1, 11, 22, payload.len() - 1]);
    frames[2].delay = Duration::from_millis(20);
    let gateway =
        MockGateway::start_ordered(vec![Step::respond(Response::sse_stream(frames))]).await;
    let started = Instant::now();

    let response = reqwest::get(gateway.base_url())
        .await
        .expect("stream request");
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let received = response.bytes().await.expect("stream body");
    assert_eq!(received.as_ref(), payload);
    assert!(started.elapsed() >= Duration::from_millis(15));
}

#[tokio::test]
async fn fixed_fragments_and_chunked_frames_preserve_payloads() {
    let fixed = MockGateway::start_ordered(vec![Step::respond(Response::new(
        200,
        Body::FixedFragments(vec![Frame::new("ab"), Frame::new("cd")]),
    ))])
    .await;
    assert_eq!(
        reqwest::get(fixed.base_url())
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "abcd"
    );

    let chunked = MockGateway::start_ordered(vec![Step::respond(Response::new(
        200,
        Body::Chunked(vec![Frame::new("alpha"), Frame::new("beta")]),
    ))])
    .await;
    assert_eq!(
        reqwest::get(chunked.base_url())
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "alphabeta"
    );
}

#[tokio::test]
async fn short_fixed_body_close_and_reset_are_detected() {
    for reset in [false, true] {
        let gateway = MockGateway::start_ordered(vec![Step::respond(Response::new(
            200,
            Body::FixedThenDrop {
                declared_len: 100,
                sent: b"short".to_vec(),
                reset,
            },
        ))])
        .await;
        let response = reqwest::get(gateway.base_url())
            .await
            .expect("response headers");
        assert!(
            response.bytes().await.is_err(),
            "short body must fail (reset={reset})"
        );
    }
}

#[tokio::test]
async fn truncated_chunked_body_is_detected() {
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::new(
        200,
        Body::ChunkedThenDrop {
            frames: vec![Frame::new("partial")],
            reset: false,
        },
    ))])
    .await;
    let response = reqwest::get(gateway.base_url())
        .await
        .expect("response headers");
    assert!(response.bytes().await.is_err());
}

#[tokio::test]
async fn close_and_reset_before_headers_fail_without_a_response() {
    for step in [Step::close(), Step::reset()] {
        let gateway = MockGateway::start_ordered(vec![step]).await;
        assert!(reqwest::get(gateway.base_url()).await.is_err());
        assert_eq!(gateway.request_count(), 1);
    }
}

#[tokio::test]
async fn connection_and_body_stalls_wait_for_caller_cancellation() {
    for step in [
        Step::stall(),
        Step::respond(Response::new(200, Body::NeverEnds)),
    ] {
        let gateway = MockGateway::start_ordered(vec![step]).await;
        let outcome = tokio::time::timeout(Duration::from_millis(40), async {
            let response = reqwest::get(gateway.base_url()).await?;
            response.bytes().await
        })
        .await;
        assert!(
            outcome.is_err(),
            "stalled request must require cancellation"
        );
    }
}

#[tokio::test]
async fn default_request_debug_never_logs_raw_auth() {
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::status_only(204))]).await;
    reqwest::Client::new()
        .get(gateway.base_url())
        .header("authorization", "Bearer must-not-appear")
        .header("x-api-key", "must-not-appear-either")
        .header("x-userid", "grok-user-must-not-appear")
        .header("x-teamid", "grok-team-must-not-appear")
        .header("x-request-id", "request-id-must-not-appear")
        .header("x-forwarded-host", "routing-must-not-appear")
        .header("x-unknown-provider-metadata", "unknown-must-not-appear")
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap();
    let rendered = format!("{:?}", gateway.requests());
    assert!(!rendered.contains("must-not-appear"));
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("application/json"));
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition should become true");
}
