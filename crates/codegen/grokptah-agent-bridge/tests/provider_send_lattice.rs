//! The provider-send lattice against synthetic dialects and crash cuts (#478).
//!
//! Every test here talks to a loopback [`MockGateway`]; nothing in this file can
//! reach a real provider, and the crash-cut helper binary refuses any URL that
//! is not an explicit HTTP loopback address.
//!
//! What the matrix is for: the lattice's whole claim is that a durable record
//! never says "not delivered" when it cannot know, and never silently opens a
//! fresh ordinal over an unresolved one. Those are claims about what survives an
//! interruption, so they are only worth the crash tests below.

use std::path::Path;
use std::time::Duration;

use grokptah_agent_bridge::provider_send::{
    self, AttemptLedger, CallSiteFamily, CrashCut, CutAction, DeliveryKnowledge, HostIncarnationId,
    ProviderAttemptProjection, ProviderAttemptState, ProviderRequestSpec, ProviderSendContext,
    ResponseAccept, SendAuthorities, SendOrigin, SendScope, SettlementOutcome, UncertaintyClass,
    WireDialect,
};
use grokptah_test_gateway::{split_evenly, Body, Frame, MockGateway, Response, Step};
use tokio_util::sync::CancellationToken;

const COMPLETION_JSON: &str = r#"{"id":"chatcmpl-synthetic-1","choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#;

fn sse_stream_bytes() -> Vec<u8> {
    let mut out = String::new();
    out.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n");
    out.push_str("data: [DONE]\n\n");
    out.into_bytes()
}

fn body() -> serde_json::Value {
    serde_json::json!({
        "model": "synthetic-model",
        "messages": [{"role": "user", "content": "synthetic"}],
        "stream": false
    })
}

fn context_at(
    root: &Path,
    session: &str,
    family: CallSiteFamily,
    origin: SendOrigin,
) -> ProviderSendContext {
    ProviderSendContext::for_root(
        root.join("provider-attempts"),
        "lattice-test",
        session,
        origin,
        family,
    )
    .expect("ledger")
}

fn spec<'a>(
    base_url: &'a str,
    payload: &'a serde_json::Value,
    dialect: WireDialect,
    accept: ResponseAccept,
) -> ProviderRequestSpec<'a> {
    ProviderRequestSpec {
        credentials: None,
        base_url,
        wire_model: "synthetic-model",
        dialect,
        credential_binding: None,
        body: payload,
        accept,
        effort_header: None,
        request_timeout: Duration::from_secs(5),
        observation: None,
    }
}

fn scope_of(context: &ProviderSendContext) -> SendScope {
    context.scope()
}

/// Reopen a scope's ledger as a *different* host incarnation, the way a restart
/// does, and run recovery.
fn restart_and_recover(
    root: &Path,
    context: &ProviderSendContext,
    incarnation: &str,
) -> (AttemptLedger, provider_send::RecoveryReport) {
    let ledger = AttemptLedger::open_as(
        root.join("provider-attempts"),
        HostIncarnationId::from_raw(incarnation),
    )
    .expect("reopen");
    let report = ledger.recover_scope(&scope_of(context)).expect("recover");
    (ledger, report)
}

// ───────────────────────── synthetic dialect matrix ─────────────────────────

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn every_dialect_settles_a_complete_json_response() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    for dialect in WireDialect::ALL {
        let gateway = MockGateway::start_ordered(vec![Step::respond(Response::json(
            200,
            &serde_json::from_str::<serde_json::Value>(COMPLETION_JSON).unwrap(),
        ))])
        .await;
        let dir = tempfile::tempdir().expect("tmp");
        let context = context_at(
            dir.path(),
            dialect.as_str(),
            CallSiteFamily::DesktopChatTurn,
            SendOrigin::Desktop,
        );
        let cancel = CancellationToken::new();
        let payload = body();

        let sent = provider_send::dispatch(
            &context,
            spec(gateway.base_url(), &payload, dialect, ResponseAccept::Json),
            &cancel,
        )
        .await
        .unwrap_or_else(|error| panic!("{} dispatch: {error}", dialect.as_str()));
        assert_eq!(sent.status(), 200);

        let mut reader = sent.into_reader();
        let raw = reader.read_to_string(&cancel).await.expect("body");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        reader
            .settle_completed(value["id"].as_str(), Some(7), Some(3), Some(10))
            .expect("settle");
        drop(reader);

        let stored = context
            .ledger()
            .load(&scope_of(&context), 1)
            .expect("load")
            .expect("present");
        assert_eq!(stored.state, ProviderAttemptState::Settled, "{dialect:?}");
        assert_eq!(
            stored.delivery_knowledge(),
            DeliveryKnowledge::KnownDelivered
        );
        let settlement = stored.settlement.expect("settlement");
        assert_eq!(settlement.outcome, SettlementOutcome::Completed);
        assert!(settlement.receipt.provider_receipt.is_some());
        assert_eq!(settlement.accounting.completion_tokens, Some(3));
        settlement.validate().expect("consistent");
    }
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn every_dialect_settles_a_complete_event_stream() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    for dialect in WireDialect::ALL {
        let frames = split_evenly(&sse_stream_bytes(), 4);
        let gateway =
            MockGateway::start_ordered(vec![Step::respond(Response::sse_stream(frames))]).await;
        let dir = tempfile::tempdir().expect("tmp");
        let context = context_at(
            dir.path(),
            dialect.as_str(),
            CallSiteFamily::DesktopBuildRound,
            SendOrigin::Desktop,
        );
        let cancel = CancellationToken::new();
        let payload = body();

        let sent = provider_send::dispatch(
            &context,
            spec(
                gateway.base_url(),
                &payload,
                dialect,
                ResponseAccept::EventStream,
            ),
            &cancel,
        )
        .await
        .expect("dispatch");
        let mut reader = sent.into_reader();
        let mut chunks = 0usize;
        while let Some(chunk) = reader.next_chunk(&cancel).await {
            chunk.expect("chunk");
            chunks += 1;
            // Bytes observed move the attempt to Responding immediately.
            assert_eq!(reader.state(), ProviderAttemptState::Responding);
            let _ = chunks;
        }
        assert!(chunks >= 1, "{dialect:?} produced no chunks");
        assert!(
            reader.stream_complete(),
            "{dialect:?} stream did not finish"
        );
        // A completed body is not yet a settled exchange: the host still has to
        // make sense of it.
        assert_eq!(reader.state(), ProviderAttemptState::Responding);
        reader
            .settle_completed(None, None, None, None)
            .expect("settle");
        assert_eq!(reader.state(), ProviderAttemptState::Settled);
    }
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn no_dialect_receives_an_undeclared_idempotency_header() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    for dialect in WireDialect::ALL {
        let gateway = MockGateway::start_ordered(vec![Step::respond(Response::json(
            200,
            &serde_json::json!({"id": "x", "choices": []}),
        ))])
        .await;
        let dir = tempfile::tempdir().expect("tmp");
        let context = context_at(
            dir.path(),
            dialect.as_str(),
            CallSiteFamily::DesktopChatTurn,
            SendOrigin::Desktop,
        );
        let cancel = CancellationToken::new();
        let payload = body();
        let sent = provider_send::dispatch(
            &context,
            spec(gateway.base_url(), &payload, dialect, ResponseAccept::Json),
            &cancel,
        )
        .await
        .expect("dispatch");
        let mut reader = sent.into_reader();
        let _ = reader.read_to_string(&cancel).await;
        let _ = reader.settle_completed(None, None, None, None);
        drop(reader);

        let request = gateway.requests().pop().expect("recorded request");
        assert!(
            request.header("idempotency-key").is_none(),
            "{} must not be sent an idempotency header it never declared",
            dialect.as_str()
        );
        // The host still has its own identity for the attempt.
        let stored = context
            .ledger()
            .load(&scope_of(&context), 1)
            .expect("load")
            .expect("present");
        assert!(stored.binding.host_key_is_rederivable());
    }
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn the_host_key_never_appears_on_the_wire_or_in_a_projection() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::json(
        200,
        &serde_json::json!({"id": "chatcmpl-private", "choices": []}),
    ))])
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "secrets",
        CallSiteFamily::DesktopChatTurn,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();
    let sent = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::Json,
        ),
        &cancel,
    )
    .await
    .expect("dispatch");
    let host_key = sent
        .attempt()
        .binding()
        .host_idempotency()
        .key()
        .as_str()
        .to_string();
    let mut reader = sent.into_reader();
    let _ = reader.read_to_string(&cancel).await;
    reader
        .settle_completed(Some("chatcmpl-private"), None, None, None)
        .expect("settle");
    drop(reader);

    let request = gateway.requests().pop().expect("recorded request");
    let serialized = format!("{:?}", request);
    assert!(
        !serialized.contains(&host_key),
        "the host idempotency key must stay host-side"
    );

    let projections = context.projections().expect("projections");
    let json = serde_json::to_string(&projections).expect("serialize");
    assert!(!json.contains(&host_key), "projection leaked the host key");
    assert!(
        !json.contains("chatcmpl-private"),
        "projection leaked the provider receipt"
    );
    assert!(
        !json.contains(gateway.base_url()),
        "projection leaked the raw route"
    );
    // Receipt presence is still stated honestly.
    assert!(
        projections[0]
            .settlement
            .as_ref()
            .expect("settlement")
            .provider_receipt_present
    );
}

// ───────────────────────── transport evidence ─────────────────────────

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_refused_connection_is_the_only_proof_of_non_delivery() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    // Bind then drop: the port is closed, so the connection is never made.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    drop(listener);

    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "refused",
        CallSiteFamily::DesktopChatTurn,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();
    let base = format!("http://{address}/v1");
    let error = provider_send::dispatch(
        &context,
        spec(
            &base,
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::Json,
        ),
        &cancel,
    )
    .await
    .expect_err("connection must be refused");

    assert_eq!(error.delivery(), DeliveryKnowledge::KnownNotDelivered);
    assert!(error.may_auto_retry(), "proven non-delivery may be retried");
    let stored = context
        .ledger()
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(stored.state, ProviderAttemptState::NotSent);
    // And the scope is free for a genuine retry.
    assert!(context
        .begin_attempt(
            provider_send::RouteIncarnation::new(
                &base,
                "synthetic-model",
                WireDialect::OpenAiChatCompletions,
                "unauthenticated",
                None,
            ),
            provider_send::RequestDigest::of_body(b"retry"),
        )
        .is_ok());
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_reset_after_the_request_is_written_stays_uncertain_and_does_not_retry() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let gateway = MockGateway::start_ordered(vec![Step::reset()]).await;
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "reset",
        CallSiteFamily::DesktopChatTurn,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();
    let error = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::Json,
        ),
        &cancel,
    )
    .await
    .expect_err("reset after write");

    assert_eq!(error.delivery(), DeliveryKnowledge::Unknown);
    assert!(
        !error.may_auto_retry(),
        "a request that may have been written must never auto-retry"
    );
    let stored = context
        .ledger()
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(stored.state, ProviderAttemptState::Uncertain);

    // And the scope refuses to silently open a fresh ordinal over it.
    let refused = context.begin_attempt(
        provider_send::RouteIncarnation::new(
            gateway.base_url(),
            "synthetic-model",
            WireDialect::OpenAiChatCompletions,
            "unauthenticated",
            None,
        ),
        provider_send::RequestDigest::of_body(b"retry"),
    );
    assert!(
        refused.is_err(),
        "an unresolved attempt must block the scope"
    );
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_body_that_ends_early_leaves_the_attempt_uncertain() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let complete = COMPLETION_JSON.as_bytes().to_vec();
    // Declares the full length, sends 20 bytes, then closes: the exact shape of
    // "the provider answered and the connection died part-way through".
    let gateway = MockGateway::start_ordered(vec![Step::respond(
        Response::new(
            200,
            Body::FixedThenDrop {
                declared_len: complete.len(),
                sent: complete[..20].to_vec(),
                reset: false,
            },
        )
        .with_header("content-type", "application/json"),
    )])
    .await;

    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "short-body",
        CallSiteFamily::DesktopChatTurn,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();
    let sent = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::Json,
        ),
        &cancel,
    )
    .await
    .expect("headers arrive");

    let mut reader = sent.into_reader();
    let outcome = reader.read_to_string(&cancel).await;
    assert!(
        outcome.is_err(),
        "a truncated body must not read as complete"
    );
    drop(reader);

    let stored = context
        .ledger()
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(stored.state, ProviderAttemptState::Uncertain);
    let settlement = stored.settlement.expect("drop settles");
    assert_eq!(settlement.outcome, SettlementOutcome::Uncertain);
    assert_eq!(
        settlement.audit,
        provider_send::AuditOutcome::Unresolved,
        "audit must stay open on an unresolved attempt"
    );
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn an_abandoned_reader_records_uncertainty_rather_than_silence() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let frames = split_evenly(&sse_stream_bytes(), 4);
    let gateway =
        MockGateway::start_ordered(vec![Step::respond(Response::sse_stream(frames))]).await;
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "abandoned",
        CallSiteFamily::DesktopBuildRound,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();
    let sent = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::EventStream,
        ),
        &cancel,
    )
    .await
    .expect("dispatch");

    // Read one chunk, then walk away — the shape of an early return on a parse
    // error deep inside a caller.
    let mut reader = sent.into_reader();
    let _ = reader.next_chunk(&cancel).await;
    drop(reader);

    let stored = context
        .ledger()
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(stored.state, ProviderAttemptState::Uncertain);
    assert_eq!(stored.delivery_knowledge(), DeliveryKnowledge::Unknown);
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_provider_rejection_settles_and_frees_the_scope() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let gateway = MockGateway::start_ordered(vec![
        Step::respond(Response::status_only(429)),
        Step::respond(Response::json(
            200,
            &serde_json::json!({"id": "after-retry", "choices": []}),
        )),
    ])
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "rejected",
        CallSiteFamily::DesktopChatTurn,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();

    let sent = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::Json,
        ),
        &cancel,
    )
    .await
    .expect("dispatch");
    assert_eq!(sent.status(), 429);
    let mut reader = sent.into_reader();
    let _ = reader.read_to_string(&cancel).await;
    reader.settle_rejected().expect("settle");
    drop(reader);

    // The provider answered, so this is not uncertainty: a *new* attempt with
    // its own ordinal is admissible.
    let second = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::Json,
        ),
        &cancel,
    )
    .await
    .expect("second attempt");
    assert_eq!(second.attempt().ordinal(), 2);
    assert_eq!(second.status(), 200);

    let first = context
        .ledger()
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(first.state, ProviderAttemptState::Settled);
    assert_eq!(
        first.settlement.expect("settlement").outcome,
        SettlementOutcome::ProviderRejected
    );
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_redirect_is_answered_by_the_provider_and_never_followed() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let gateway = MockGateway::start_ordered(vec![Step::respond(
        Response::status_only(302).with_header("location", "http://elsewhere.invalid/v1"),
    )])
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "redirect",
        CallSiteFamily::DesktopChatTurn,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();
    let sent = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::Json,
        ),
        &cancel,
    )
    .await
    .expect("redirect arrives as a response");

    // Redirects are disabled on the client: a 3xx is the provider answering,
    // not an invitation to send the same request to a second, unbound endpoint.
    assert_eq!(sent.status(), 302);
    let mut reader = sent.into_reader();
    let _ = reader.read_to_string(&cancel).await;
    reader.settle_rejected().expect("settle");
    drop(reader);

    assert_eq!(
        gateway.request_count(),
        1,
        "the redirect must not be followed"
    );
    let stored = context
        .ledger()
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(
        stored.delivery_knowledge(),
        DeliveryKnowledge::KnownDelivered
    );
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_stream_reconnect_is_a_new_ordinal_not_a_reopened_one() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let gateway = MockGateway::start_ordered(vec![
        // First connection: headers, one frame, then the peer closes.
        Step::respond(Response::sse_stream(vec![Frame::new(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".to_vec(),
        )])),
        Step::respond(Response::sse_stream(split_evenly(&sse_stream_bytes(), 2))),
    ])
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "reconnect",
        CallSiteFamily::DesktopBuildRound,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();

    let sent = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::EventStream,
        ),
        &cancel,
    )
    .await
    .expect("dispatch");
    let mut reader = sent.into_reader();
    while reader.next_chunk(&cancel).await.is_some() {}
    // The stream ended without a terminal marker as far as the caller is
    // concerned; the caller declares that unusable.
    reader
        .settle_uncertain(UncertaintyClass::UnexpectedEof)
        .expect("settle");
    drop(reader);

    // A reconnect must not reopen ordinal 1: the first attempt's delivery is
    // recorded, and reusing its identity would erase that.
    let refused = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            ResponseAccept::EventStream,
        ),
        &cancel,
    )
    .await;
    assert!(
        refused.is_err(),
        "a reconnect over an unresolved attempt must be refused, not silently renumbered"
    );
}

// ───────────────────────── crash cuts ─────────────────────────

/// Run one in-process cut and report what the durable record shows afterwards.
async fn cut_and_inspect(
    cut: CrashCut,
    steps: Vec<Step>,
    accept: ResponseAccept,
) -> (
    tempfile::TempDir,
    ProviderSendContext,
    Option<ProviderAttemptState>,
) {
    let gateway = MockGateway::start_ordered(steps).await;
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        cut.as_str(),
        CallSiteFamily::DesktopBuildRound,
        SendOrigin::Desktop,
    );
    let cancel = CancellationToken::new();
    let payload = body();

    provider_send::arm_crash_cut(cut, CutAction::Interrupt);
    if let Ok(sent) = provider_send::dispatch(
        &context,
        spec(
            gateway.base_url(),
            &payload,
            WireDialect::OpenAiChatCompletions,
            accept,
        ),
        &cancel,
    )
    .await
    {
        {
            let mut reader = sent.into_reader();
            // Drive the reader far enough for a body/stream cut to fire. Only
            // settle when the read actually succeeded: a cut that fired part-way
            // is precisely the case where the host must NOT claim completion.
            let read_ok = match accept {
                ResponseAccept::Json => reader.read_to_string(&cancel).await.is_ok(),
                ResponseAccept::EventStream => {
                    let mut ok = true;
                    while let Some(chunk) = reader.next_chunk(&cancel).await {
                        if chunk.is_err() {
                            ok = false;
                            break;
                        }
                    }
                    ok
                }
            };
            if read_ok {
                // A settlement cut fires inside `settle`.
                let _ = reader.settle_completed(None, None, None, None);
            }
        }
    }
    provider_send::disarm_crash_cut();

    let state = context
        .ledger()
        .load(&scope_of(&context), 1)
        .expect("load")
        .map(|record| record.state);
    (dir, context, state)
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_cut_before_intent_leaves_nothing_durable() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let (_dir, context, state) = cut_and_inspect(
        CrashCut::BeforeIntent,
        vec![Step::respond(Response::json(200, &serde_json::json!({})))],
        ResponseAccept::Json,
    )
    .await;
    assert_eq!(state, None, "no intent means no record");
    assert_eq!(
        context
            .ledger()
            .max_ordinal(&scope_of(&context))
            .expect("max"),
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_cut_after_preparing_is_provably_not_sent_on_restart() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let (dir, context, state) = cut_and_inspect(
        CrashCut::AfterPreparing,
        vec![Step::respond(Response::json(200, &serde_json::json!({})))],
        ResponseAccept::Json,
    )
    .await;
    assert_eq!(state, Some(ProviderAttemptState::Preparing));

    let (ledger, report) = restart_and_recover(dir.path(), &context, "restarted");
    assert_eq!(report.resolved_not_sent, vec![1]);
    assert!(report.left_uncertain.is_empty());
    let recovered = ledger
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(recovered.state, ProviderAttemptState::NotSent);
    assert!(
        recovered.may_auto_retry(),
        "a record that never reached Sending is safe to retry"
    );
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn cuts_at_or_after_sending_stay_uncertain_on_restart() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    for (cut, steps, accept) in [
        (
            CrashCut::AfterSendingBeforeBytes,
            vec![Step::respond(Response::json(200, &serde_json::json!({})))],
            ResponseAccept::Json,
        ),
        (
            CrashCut::AfterBytesNoHeaders,
            vec![Step::respond(Response::json(200, &serde_json::json!({})))],
            ResponseAccept::Json,
        ),
        (
            CrashCut::AfterHeaders,
            vec![Step::respond(Response::json(200, &serde_json::json!({})))],
            ResponseAccept::Json,
        ),
        (
            CrashCut::AfterBody,
            vec![Step::respond(Response::json(200, &serde_json::json!({})))],
            ResponseAccept::Json,
        ),
        (
            CrashCut::MidStream,
            vec![Step::respond(Response::sse_stream(split_evenly(
                &sse_stream_bytes(),
                4,
            )))],
            ResponseAccept::EventStream,
        ),
    ] {
        let (dir, context, _state) = cut_and_inspect(cut, steps, accept).await;
        let (ledger, report) = restart_and_recover(dir.path(), &context, "restarted");
        let recovered = ledger
            .load(&scope_of(&context), 1)
            .expect("load")
            .expect("present");
        assert_eq!(
            recovered.state,
            ProviderAttemptState::Uncertain,
            "cut {} must leave the attempt uncertain, not resolved",
            cut.as_str()
        );
        assert!(
            !recovered.may_auto_retry(),
            "cut {} must not permit an automatic retry",
            cut.as_str()
        );
        assert!(
            report.left_uncertain.contains(&1) || report.already_terminal.is_empty(),
            "cut {} recovery report: {report:?}",
            cut.as_str()
        );
        // The scope stays blocked until the uncertainty is resolved.
        assert!(
            ledger
                .begin_attempt(context.binding_spec(
                    provider_send::RouteIncarnation::new(
                        "http://127.0.0.1:1/v1",
                        "synthetic-model",
                        WireDialect::OpenAiChatCompletions,
                        "unauthenticated",
                        None,
                    ),
                    provider_send::RequestDigest::of_body(b"next"),
                ))
                .is_err(),
            "cut {} must block a fresh ordinal",
            cut.as_str()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_cut_during_the_write_never_reaches_sending() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    // MidWrite fires between building the request and persisting `Sending`, so
    // the durable ordering proves nothing moved.
    let (dir, context, state) = cut_and_inspect(
        CrashCut::MidWrite,
        vec![Step::respond(Response::json(200, &serde_json::json!({})))],
        ResponseAccept::Json,
    )
    .await;
    assert_eq!(state, Some(ProviderAttemptState::NotSent));

    let (ledger, report) = restart_and_recover(dir.path(), &context, "restarted");
    assert_eq!(report.already_terminal, vec![1]);
    let recovered = ledger
        .load(&scope_of(&context), 1)
        .expect("load")
        .expect("present");
    assert_eq!(recovered.state, ProviderAttemptState::NotSent);
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_settlement_cut_leaves_no_partial_bundle() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    for cut in [
        CrashCut::SettlementBeforeReceipt,
        CrashCut::SettlementBeforeAudit,
    ] {
        let (dir, context, _state) = cut_and_inspect(
            cut,
            vec![Step::respond(Response::json(
                200,
                &serde_json::from_str::<serde_json::Value>(COMPLETION_JSON).unwrap(),
            ))],
            ResponseAccept::Json,
        )
        .await;

        let stored = context
            .ledger()
            .load(&scope_of(&context), 1)
            .expect("load")
            .expect("present");
        // The bundle is one atomic write: it either landed whole or not at all.
        match stored.settlement.as_ref() {
            None => {}
            Some(settlement) => settlement
                .validate()
                .unwrap_or_else(|error| panic!("cut {} left a torn bundle: {error}", cut.as_str())),
        }

        let (ledger, _) = restart_and_recover(dir.path(), &context, "restarted");
        let recovered = ledger
            .load(&scope_of(&context), 1)
            .expect("load")
            .expect("present");
        if let Some(settlement) = recovered.settlement.as_ref() {
            settlement.validate().expect("consistent after restart");
        } else {
            assert_eq!(
                recovered.state,
                ProviderAttemptState::Uncertain,
                "an unsettled delivered attempt must recover as uncertain"
            );
        }
    }
}

// ───────────── process kill and two-process restart ─────────────

fn crash_cut_helper() -> std::path::PathBuf {
    // The integration test binary lives in target/<profile>/deps; the example
    // sits one level up.
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("examples").join("provider_send_crash_cut")
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_killed_process_leaves_a_recoverable_not_sent_record() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let helper = crash_cut_helper();
    if !helper.exists() {
        // `cargo test` builds examples for --all-targets; a bare `cargo test
        // --test ...` may not. Say so rather than passing vacuously.
        eprintln!(
            "skipping: build examples first (cargo test --all-targets), missing {}",
            helper.display()
        );
        return;
    }
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::json(
        200,
        &serde_json::json!({"id": "x", "choices": []}),
    ))])
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().join("provider-attempts");

    let status = std::process::Command::new(&helper)
        .arg(&root)
        .arg("killed-session")
        .arg(gateway.base_url())
        .arg(CrashCut::AfterPreparing.as_str())
        .status()
        .expect("spawn crash-cut helper");
    assert!(
        !status.success(),
        "the helper must die at the cut, not exit cleanly"
    );

    // Second process: reopen the same ledger and recover.
    let ledger =
        AttemptLedger::open_as(&root, HostIncarnationId::from_raw("second-process")).expect("open");
    let scope = SendScope::new(
        "crash-cut-helper",
        "killed-session",
        None,
        SendOrigin::Desktop,
        CallSiteFamily::DesktopBuildRound,
    )
    .expect("scope");
    assert_eq!(ledger.max_ordinal(&scope).expect("max"), Some(1));
    let report = ledger.recover_scope(&scope).expect("recover");
    assert_eq!(report.resolved_not_sent, vec![1]);
    let recovered = ledger.load(&scope, 1).expect("load").expect("present");
    assert_eq!(recovered.state, ProviderAttemptState::NotSent);
    assert_eq!(
        gateway.request_count(),
        0,
        "a process killed at Preparing cannot have sent anything"
    );
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_killed_process_after_sending_stays_uncertain_across_a_restart() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let helper = crash_cut_helper();
    if !helper.exists() {
        eprintln!(
            "skipping: build examples first (cargo test --all-targets), missing {}",
            helper.display()
        );
        return;
    }
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::json(
        200,
        &serde_json::json!({"id": "x", "choices": []}),
    ))])
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().join("provider-attempts");

    let status = std::process::Command::new(&helper)
        .arg(&root)
        .arg("uncertain-session")
        .arg(gateway.base_url())
        .arg(CrashCut::AfterSendingBeforeBytes.as_str())
        .status()
        .expect("spawn crash-cut helper");
    assert!(!status.success(), "the helper must die at the cut");

    let scope = SendScope::new(
        "crash-cut-helper",
        "uncertain-session",
        None,
        SendOrigin::Desktop,
        CallSiteFamily::DesktopBuildRound,
    )
    .expect("scope");

    // Two separate later processes both reach the same conclusion, and neither
    // invents a fresh ordinal.
    for incarnation in ["second-process", "third-process"] {
        let ledger =
            AttemptLedger::open_as(&root, HostIncarnationId::from_raw(incarnation)).expect("open");
        ledger.recover_scope(&scope).expect("recover");
        let recovered = ledger.load(&scope, 1).expect("load").expect("present");
        assert_eq!(recovered.state, ProviderAttemptState::Uncertain);
        assert!(!recovered.may_auto_retry());
        assert_eq!(ledger.max_ordinal(&scope).expect("max"), Some(1));
    }
}

#[tokio::test(flavor = "multi_thread")]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn takeover_after_a_kill_is_idempotent_across_processes() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let helper = crash_cut_helper();
    if !helper.exists() {
        eprintln!("skipping: examples not built");
        return;
    }
    let gateway = MockGateway::start_ordered(vec![Step::respond(Response::json(
        200,
        &serde_json::json!({"id": "x", "choices": []}),
    ))])
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().join("provider-attempts");
    let status = std::process::Command::new(&helper)
        .arg(&root)
        .arg("takeover-session")
        .arg(gateway.base_url())
        .arg(CrashCut::AfterPreparing.as_str())
        .status()
        .expect("spawn");
    assert!(!status.success());

    let scope = SendScope::new(
        "crash-cut-helper",
        "takeover-session",
        None,
        SendOrigin::Desktop,
        CallSiteFamily::DesktopBuildRound,
    )
    .expect("scope");
    let ledger = AttemptLedger::open_as(&root, HostIncarnationId::from_raw("taker")).expect("open");
    let first = ledger.takeover(&scope, 1).expect("takeover");
    assert!(matches!(
        first,
        provider_send::TakeoverOutcome::Claimed { .. }
    ));
    let revision = ledger
        .load(&scope, 1)
        .expect("load")
        .expect("present")
        .revision;
    let second = ledger.takeover(&scope, 1).expect("takeover");
    assert!(matches!(
        second,
        provider_send::TakeoverOutcome::AlreadyOwned { .. }
    ));
    assert_eq!(
        ledger
            .load(&scope, 1)
            .expect("load")
            .expect("present")
            .revision,
        revision,
        "an idempotent takeover writes nothing"
    );
}

// ───────────────────────── call-site families ─────────────────────────

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn every_call_site_family_can_open_a_bound_attempt() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let gateway = MockGateway::start_routed(|_| {
        Step::respond(Response::json(
            200,
            &serde_json::json!({"id": "x", "choices": []}),
        ))
    })
    .await;
    let dir = tempfile::tempdir().expect("tmp");
    let cancel = CancellationToken::new();
    let payload = body();

    for family in CallSiteFamily::ALL {
        for origin in SendOrigin::ALL {
            let context = ProviderSendContext::new(
                std::sync::Arc::new(
                    AttemptLedger::open(dir.path().join("provider-attempts")).expect("ledger"),
                ),
                "families",
                "session",
                Some("run"),
                origin,
                family,
                SendAuthorities::provisional("provider", "account", "policy"),
            )
            .expect("context");

            let sent = provider_send::dispatch(
                &context,
                spec(
                    gateway.base_url(),
                    &payload,
                    WireDialect::OpenAiChatCompletions,
                    ResponseAccept::Json,
                ),
                &cancel,
            )
            .await
            .unwrap_or_else(|error| panic!("{family:?}/{origin:?} must be able to send: {error}"));

            // Each family/origin pair is its own ordinal sequence, so the first
            // attempt in each is ordinal 1.
            assert_eq!(sent.attempt().ordinal(), 1, "{family:?}/{origin:?}");
            let mut reader = sent.into_reader();
            let _ = reader.read_to_string(&cancel).await;
            reader
                .settle_completed(None, None, None, None)
                .expect("settle");
        }
    }

    assert_eq!(
        gateway.request_count(),
        CallSiteFamily::ALL.len() * SendOrigin::ALL.len()
    );
}

#[tokio::test]
// Deliberate: crash-cut arming is process-global, so the guard has to outlive
// the awaits it is protecting.
#[allow(clippy::await_holding_lock)]
async fn a_projection_of_every_state_is_redacted_and_honest() {
    // Crash-cut arming is process-global: every test in this binary
    // serializes on it, or a cut armed by one fires inside another.
    let _cuts = provider_send::crash_cut_test_lock();
    let dir = tempfile::tempdir().expect("tmp");
    let context = context_at(
        dir.path(),
        "private-session-marker",
        CallSiteFamily::ExploreSubagent,
        SendOrigin::Orchestration,
    );
    let handle = context
        .begin_attempt(
            provider_send::RouteIncarnation::new(
                "https://private.gateway.invalid/secret-path",
                "operator-model",
                WireDialect::OpenAiChatCompletions,
                "gateway_api_key",
                Some("credential-binding-secret"),
            ),
            provider_send::RequestDigest::of_body(b"a private prompt"),
        )
        .expect("begin");
    let projection = ProviderAttemptProjection::of(handle.record());
    let json = serde_json::to_string(&projection).expect("serialize");
    for secret in [
        "private.gateway.invalid",
        "secret-path",
        "credential-binding-secret",
        "a private prompt",
        "private-session-marker",
    ] {
        assert!(!json.contains(secret), "projection leaked {secret}");
    }
    assert!(json.contains("operator-model"), "the model stays visible");
    assert_eq!(projection.delivery, DeliveryKnowledge::KnownNotDelivered);
    assert!(projection.summary().contains("did not reach"));
}
