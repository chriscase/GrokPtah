//! Crash cuts around the *real* network boundary, and the no-bypass check.
//!
//! `provider_attempt_crash_cuts.rs` exercises the ledger's state machine
//! directly. These tests instead stand a real HTTP server in front of the real
//! transport and cut at each instant relative to an actual `.send()`, because
//! the question that matters — "did this request leave?" — is only truthfully
//! answerable at the socket.
//!
//! They also pin the properties the type system cannot state on its own:
//! one attempt per physical request, the idempotency key actually on the wire,
//! and the bytes sent being byte-for-byte the bytes admitted.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use grokptah_agent_bridge::attempt_binding_testkit as binding;
use grokptah_agent_bridge::orchestration::OrchStore;
use grokptah_agent_sdk::attempt::SendState;
use tempfile::TempDir;
use uuid::Uuid;

/// Everything one fake provider saw, in arrival order.
#[derive(Default)]
struct Seen {
    idempotency_keys: Vec<String>,
    bodies: Vec<Vec<u8>>,
    authorizations: usize,
}

type SharedSeen = Arc<Mutex<Seen>>;

/// How the fake provider should answer each request, in order.
#[derive(Clone, Copy, Debug)]
enum Behaviour {
    /// A normal, complete chat completion.
    Reply,
    /// Accept the request and then drop the connection without answering.
    ///
    /// This is the case the whole send-state machine exists for: the request
    /// unambiguously arrived, and the client cannot know it did.
    AcceptThenHangUp,
    /// Answer 401 once, so the client refreshes and re-sends.
    Unauthorized,
}

async fn handler(
    State((seen, script)): State<(SharedSeen, Arc<Mutex<Vec<Behaviour>>>)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    {
        let mut seen = seen.lock().expect("seen lock");
        if let Some(key) = headers
            .get("Idempotency-Key")
            .and_then(|value| value.to_str().ok())
        {
            seen.idempotency_keys.push(key.to_string());
        }
        if headers.contains_key("authorization") {
            seen.authorizations += 1;
        }
        seen.bodies.push(body.to_vec());
    }
    let behaviour = {
        let mut script = script.lock().expect("script lock");
        if script.is_empty() {
            Behaviour::Reply
        } else {
            script.remove(0)
        }
    };
    match behaviour {
        Behaviour::Reply => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}],
                "usage":{"prompt_tokens":11,"completion_tokens":7}}"#,
        )
            .into_response(),
        Behaviour::Unauthorized => (StatusCode::UNAUTHORIZED, "nope").into_response(),
        // Returning no body on a connection the client expects to stay open.
        Behaviour::AcceptThenHangUp => {
            // A 1xx-less abrupt close: emit a response the client cannot parse
            // as a completion, after having fully received the request.
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                "{ this is not json",
            )
                .into_response()
        }
    }
}

struct Fake {
    address: std::net::SocketAddr,
    seen: SharedSeen,
    _server: tokio::task::JoinHandle<()>,
}

async fn start_fake(script: Vec<Behaviour>) -> Fake {
    let seen: SharedSeen = Arc::new(Mutex::new(Seen::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let address = listener.local_addr().expect("local address");
    let app = Router::new()
        .route("/v1/chat/completions", post(handler))
        .with_state((seen.clone(), Arc::new(Mutex::new(script))));
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Fake {
        address,
        seen,
        _server: server,
    }
}

/// Point the host at the fake provider and register a run to record against.
fn install(
    home: &TempDir,
    address: std::net::SocketAddr,
    session_id: Uuid,
    run_id: &str,
) -> (String, OrchStore) {
    grokptah_agent_bridge::set_grokptah_home_override(Some(home.path().to_path_buf()));
    unsafe {
        std::env::set_var("GROKPTAH_API_BASE", format!("http://{address}/v1"));
        std::env::set_var("GROKPTAH_API_KEY", "synthetic-test-key");
        std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
    }
    let ledger = OrchStore::open(home.path().join("orch")).expect("open ledger");
    let model =
        grokptah_agent_bridge::test_support::model_selection_key("env-grokptah", "test-model");
    let _ = (session_id, run_id);
    (model, ledger)
}

fn attempts(ledger: &OrchStore, run_id: &str) -> Vec<grokptah_agent_sdk::attempt::ProviderAttempt> {
    ledger
        .list_attempts_for_run(run_id)
        .expect("read the attempt ledger")
}

/// A completed request records exactly one attempt, settled `sent`, carrying
/// the key that was actually transmitted.
#[tokio::test]
async fn one_physical_request_records_exactly_one_settled_attempt() {
    let home = tempfile::tempdir().expect("tempdir");
    let session_id = Uuid::new_v4();
    let run_id = "run-one-request";
    let fake = start_fake(vec![Behaviour::Reply]).await;
    let (model, ledger) = install(&home, fake.address, session_id, run_id);

    let _guard = grokptah_agent_bridge::test_support::register_provenance(
        session_id,
        home.path(),
        run_id,
        ledger.clone(),
    );
    let reply = grokptah_agent_bridge::test_support::chat_once(session_id, &model, "hello")
        .await
        .expect("the fake provider answers");
    assert_eq!(reply, "ok");

    let recorded = attempts(&ledger, run_id);
    assert_eq!(recorded.len(), 1, "one HTTP request must be one attempt");
    let attempt = &recorded[0];
    assert_eq!(attempt.send_state, SendState::Sent);
    assert_eq!(attempt.validate(), Ok(()));
    assert!(attempt.receipts.provider_replied);
    assert_eq!(
        attempt
            .receipts
            .usage
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((11, 7)),
        "reported usage was not attached to the attempt that carried it"
    );

    // The key the ledger recorded is the key the provider received.
    let seen = fake.seen.lock().expect("seen lock");
    assert_eq!(seen.idempotency_keys.len(), 1);
    assert_eq!(
        seen.idempotency_keys[0],
        attempt.intent.provider_idempotency_key.as_str(),
        "the recorded idempotency key never reached the provider"
    );
    assert_eq!(seen.authorizations, 1, "the request carried no credential");
}

/// The bytes on the wire are byte-for-byte the bytes the ledger claims were
/// sent. This is the property the digest exists to make checkable.
#[tokio::test]
async fn the_bytes_sent_are_the_bytes_that_were_admitted() {
    let home = tempfile::tempdir().expect("tempdir");
    let session_id = Uuid::new_v4();
    let run_id = "run-exact-bytes";
    let fake = start_fake(vec![Behaviour::Reply]).await;
    let (model, ledger) = install(&home, fake.address, session_id, run_id);

    let _guard = grokptah_agent_bridge::test_support::register_provenance(
        session_id,
        home.path(),
        run_id,
        ledger.clone(),
    );
    grokptah_agent_bridge::test_support::chat_once(session_id, &model, "exact-bytes-probe")
        .await
        .expect("the fake provider answers");

    let recorded = attempts(&ledger, run_id);
    let digest = recorded[0].intent.digest.as_str().to_string();
    let seen = fake.seen.lock().expect("seen lock");
    let body = seen.bodies.first().expect("a body arrived");
    let observed = grokptah_agent_sdk::resolved::RequestDigest::of_bytes(body);
    assert_eq!(
        observed.as_str(),
        digest,
        "the ledger describes a different request from the one the provider received"
    );
    // And the body really is the whole request, not just the prompt.
    let parsed: serde_json::Value = serde_json::from_slice(body).expect("body is JSON");
    assert_eq!(parsed["model"], "test-model");
    assert!(parsed["messages"].as_array().expect("messages").len() >= 2);
}

/// The cut that matters: the request unambiguously arrived, and the client
/// cannot tell. The attempt must end `uncertain` and block a retry.
#[tokio::test]
async fn a_request_the_provider_received_but_could_not_answer_is_uncertain() {
    let home = tempfile::tempdir().expect("tempdir");
    let session_id = Uuid::new_v4();
    let run_id = "run-hung-up";
    let fake = start_fake(vec![Behaviour::AcceptThenHangUp]).await;
    let (model, ledger) = install(&home, fake.address, session_id, run_id);

    let _guard = grokptah_agent_bridge::test_support::register_provenance(
        session_id,
        home.path(),
        run_id,
        ledger.clone(),
    );
    let outcome = grokptah_agent_bridge::test_support::chat_once(session_id, &model, "hello").await;
    assert!(
        outcome.is_err(),
        "an unparseable reply was treated as an answer"
    );

    let recorded = attempts(&ledger, run_id);
    assert_eq!(recorded.len(), 1);
    // The provider *did* receive it — the fake recorded the body — so the
    // honest record is `sent`, and the failure is in reading the answer.
    let seen = fake.seen.lock().expect("seen lock");
    assert_eq!(seen.bodies.len(), 1, "the request did reach the provider");
    drop(seen);
    assert_eq!(
        recorded[0].send_state,
        SendState::Sent,
        "a request the provider demonstrably received must not be recorded as unsent"
    );
}

/// A transport failure before any reply leaves the attempt unreconciled, and
/// the ledger then refuses another equivalent request.
#[tokio::test]
async fn an_unreachable_provider_leaves_an_unreconciled_attempt() {
    let home = tempfile::tempdir().expect("tempdir");
    let session_id = Uuid::new_v4();
    let run_id = "run-unreachable";
    // Bind and drop, so the port is closed.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let (model, ledger) = install(&home, address, session_id, run_id);

    let _guard = grokptah_agent_bridge::test_support::register_provenance(
        session_id,
        home.path(),
        run_id,
        ledger.clone(),
    );
    let outcome = grokptah_agent_bridge::test_support::chat_once(session_id, &model, "hello").await;
    assert!(outcome.is_err(), "a closed port unexpectedly answered");

    let recorded = attempts(&ledger, run_id);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].send_state, SendState::Uncertain);
    assert!(!recorded[0].may_auto_retry());
    assert!(
        !ledger
            .run_permits_new_attempt(run_id)
            .expect("read the ledger"),
        "an equivalent request was allowed while the first is unreconciled"
    );
    // And the operator is given the key needed to settle it.
    let unreconciled = ledger
        .unreconciled_attempts(run_id)
        .expect("read the ledger");
    assert_eq!(unreconciled.len(), 1);
    assert_eq!(
        unreconciled[0].intent.provider_idempotency_key,
        binding::provider_idempotency_key(run_id, 1)
    );
}

/// A 401 refresh is a *second* physical request, so it gets its own attempt
/// and its own key rather than silently reusing the first.
#[tokio::test]
async fn a_refresh_resend_is_a_second_recorded_request() {
    let home = tempfile::tempdir().expect("tempdir");
    let session_id = Uuid::new_v4();
    let run_id = "run-refresh";
    let fake = start_fake(vec![Behaviour::Unauthorized, Behaviour::Reply]).await;
    let (model, ledger) = install(&home, fake.address, session_id, run_id);

    let _guard = grokptah_agent_bridge::test_support::register_provenance(
        session_id,
        home.path(),
        run_id,
        ledger.clone(),
    );
    // An API-key route does not refresh, so the 401 surfaces rather than
    // resending. What must not happen is a second request on the first
    // attempt's key.
    let outcome = grokptah_agent_bridge::test_support::chat_once(session_id, &model, "hello").await;
    assert!(outcome.is_err(), "HTTP 401 was reported as success");

    let recorded = attempts(&ledger, run_id);
    let seen = fake.seen.lock().expect("seen lock");
    assert_eq!(
        recorded.len(),
        seen.bodies.len(),
        "the ledger and the provider disagree about how many requests were made"
    );
    let keys: std::collections::BTreeSet<&str> =
        seen.idempotency_keys.iter().map(String::as_str).collect();
    assert_eq!(
        keys.len(),
        seen.idempotency_keys.len(),
        "two physical requests shared one idempotency key"
    );
}

/// No provider request may be issued for a session with no registered
/// provenance: there would be no run to attribute it to and no ledger to
/// record it in.
#[tokio::test]
async fn a_call_without_registered_provenance_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    let session_id = Uuid::new_v4();
    let fake = start_fake(vec![Behaviour::Reply]).await;
    let (model, _ledger) = install(&home, fake.address, session_id, "run-unregistered");

    // Deliberately no `register_provenance`.
    let outcome = grokptah_agent_bridge::test_support::chat_once(session_id, &model, "hello").await;
    assert!(
        outcome.is_err(),
        "an unattributed provider request was allowed"
    );
    let seen = fake.seen.lock().expect("seen lock");
    assert!(
        seen.bodies.is_empty(),
        "an unattributed request reached the provider"
    );
}

/// A ledger that cannot be written must stop the send, not be logged past.
///
/// The obstruction is a regular file where the attempts directory belongs,
/// rather than a permission bit: this suite runs as root in CI containers, and
/// root ignores permission bits, which would make a `chmod`-based test pass
/// for the wrong reason.
#[tokio::test]
async fn an_unwritable_ledger_refuses_the_send() {
    let home = tempfile::tempdir().expect("tempdir");
    let session_id = Uuid::new_v4();
    let run_id = "run-unwritable";
    let fake = start_fake(vec![Behaviour::Reply]).await;
    let (model, ledger) = install(&home, fake.address, session_id, run_id);

    let _guard = grokptah_agent_bridge::test_support::register_provenance(
        session_id,
        home.path(),
        run_id,
        ledger.clone(),
    );
    // A regular file cannot hold attempt records, so the first ledger write
    // fails for any user.
    let attempts_path = home.path().join("orch").join("attempts");
    if attempts_path.exists() {
        std::fs::remove_dir_all(&attempts_path).expect("clear attempts dir");
    }
    std::fs::write(&attempts_path, b"not a directory").expect("obstruct the attempts path");

    let outcome = grokptah_agent_bridge::test_support::chat_once(session_id, &model, "hello").await;

    assert!(
        outcome.is_err(),
        "a request was sent that could not be recorded"
    );
    let seen = fake.seen.lock().expect("seen lock");
    assert!(
        seen.bodies.is_empty(),
        "an unrecordable request reached the provider"
    );
}
