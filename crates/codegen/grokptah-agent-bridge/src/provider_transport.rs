//! The only place a provider request physically leaves this host.
//!
//! Every byte that reaches a provider passes through [`send_admitted`], which
//! takes an [`AdmittedCall`] and has no other way to learn a URL, a
//! credential, or a body. A caller that has not been admitted cannot construct
//! the argument, so an unadmitted send does not typecheck.
//!
//! # One call here is one HTTP request
//!
//! The previous revision put a four-iteration retry loop *and* a 401-refresh
//! resend inside the transport, so a single "attempt" record could stand for
//! five physical requests, each independently capable of having executed. Here
//! each physical request opens its own attempt, with its own ordinal and its
//! own idempotency key, and retry is a decision the caller makes by calling
//! again — only when the ledger says the previous request provably never left.
//!
//! # Ordering around `.send()`
//!
//! ```text
//!   verify bytes match the sealed digest
//!   persist attempt as known_not_sent   ── crash here: safe to retry
//!   persist attempt as sending          ── crash here: never auto-retried
//!   .send()                             ── the request may now exist
//!   settle sent / uncertain
//! ```
//!
//! Every one of those ledger writes fails closed. A request that cannot be
//! recorded is not sent, because a delivered request with no record is exactly
//! the duplicate-charge case the ledger exists to prevent.

use anyhow::{anyhow, Result};
use grokptah_agent_sdk::attempt::{BoundedId, ProviderReceipts};
use grokptah_agent_sdk::outcome::RunFailureKind;
use tokio_util::sync::CancellationToken;

use crate::attempt_binding;
use crate::request_admission::AdmittedCall;

/// Header carrying the idempotency key to the provider.
///
/// The previous revision derived a key, recorded it, and never put it on the
/// wire — which made it a note to ourselves rather than something the provider
/// could use to collapse a duplicate.
pub(crate) const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// Response headers a provider may use to identify a request, most specific
/// first. Recorded as opaque receipts so an uncertain attempt can be
/// reconciled by asking the provider about *this* request.
const REQUEST_ID_HEADERS: [&str; 5] = [
    "x-request-id",
    "x-grok-request-id",
    "request-id",
    "cf-ray",
    "x-amzn-requestid",
];

/// What a physical send produced.
pub(crate) struct SentResponse {
    /// The provider's response, for the caller to read.
    pub response: reqwest::Response,
    /// The attempt this request was recorded under, so usage read from the
    /// body later attaches to the exact request that produced it.
    pub attempt_id: BoundedId,
}

/// Send exactly the admitted bytes, once, recording the delivery question.
///
/// `accept` selects the response mode the caller will read (streaming or
/// whole-body); it does not change the request body, which is fixed at
/// admission.
pub(crate) async fn send_admitted(
    call: &mut AdmittedCall,
    accept: &str,
    cancel: &CancellationToken,
) -> Result<SentResponse> {
    // The bytes about to be handed to the transport are checked against the
    // digest the ledger will claim was sent, rather than assumed to match.
    call.verify_intact()?;

    if !call.permits_another_request()? {
        return Err(anyhow!(
            "a previous provider attempt for this run has an unknown outcome; \
             reconcile it against its idempotency key before issuing another request"
        ));
    }

    // Recorded before anything can leave. A crash between here and `.send()`
    // leaves `known_not_sent`, the only state that is safe to retry.
    let attempt = call.open_attempt()?;
    let idempotency_key = attempt.intent.provider_idempotency_key.as_str().to_string();

    let target = call.target();
    let base = target.base_url.clone();
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let timeout = target.deadline_class.agent_timeout();
    let dialect = target.dialect;

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!(
            "grok/{} (GrokPtah)",
            crate::auth_store::client_version_header()
        ))
        .build()
        .map_err(|error| anyhow!(error))?;

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", accept)
        // Transmitted, not merely recorded: this is what lets the provider
        // collapse a duplicate we could not rule out.
        .header(IDEMPOTENCY_HEADER, &idempotency_key);
    if dialect == crate::gateway_config::ProviderDialect::XaiChatCompletions {
        request = request.header("x-grok-effort", call.binding().effort.as_str());
    }
    let request = crate::auth_store::apply_auth_headers(request, call.credentials(), &base);
    // The exact admitted bytes. Not a re-serialization of a structure that
    // might since have changed.
    let request = request.body(call.body().to_vec());

    // The send boundary. Recorded first, and fails closed: if this write does
    // not land we must not send, because we would be unable to say afterwards
    // that we had.
    call.begin_send(&attempt)?;

    let outcome = tokio::select! {
        result = request.send() => result,
        _ = cancel.cancelled() => {
            // Cancelling does not un-send a request that already left.
            call.settle_uncertain(&attempt, RunFailureKind::TransportError)?;
            return Err(anyhow!("cancelled"));
        }
    };

    match outcome {
        Ok(response) => {
            call.settle_sent(&attempt, receipts_from(&response))?;
            Ok(SentResponse {
                response,
                attempt_id: attempt.attempt_id.clone(),
            })
        }
        Err(error) => {
            // A connect failure provably never delivered a body; anything
            // later — a timeout above all — may well have executed. Both are
            // recorded as uncertain rather than guessed at, because `reqwest`
            // cannot tell us how far a request got once it is on the wire.
            call.settle_uncertain(&attempt, classify_transport(&error))?;
            Err(anyhow!("{}", describe_transport(&error, dialect)))
        }
    }
}

/// Capture opaque provider identifiers from a response.
fn receipts_from(response: &reqwest::Response) -> ProviderReceipts {
    let request = REQUEST_ID_HEADERS.iter().find_map(|name| {
        response
            .headers()
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .and_then(BoundedId::new)
    });
    ProviderReceipts {
        request,
        run: None,
        usage: None,
        // A reply arrived and parsed as an HTTP response. That is
        // acknowledgement even when the provider names no identifier.
        provider_replied: true,
    }
}

/// Map a transport failure onto the typed vocabulary.
fn classify_transport(error: &reqwest::Error) -> RunFailureKind {
    if error.is_timeout() || error.is_connect() || error.is_request() {
        RunFailureKind::TransportError
    } else {
        RunFailureKind::ProviderError
    }
}

/// A share-safe description of a transport failure.
fn describe_transport(
    error: &reqwest::Error,
    dialect: crate::gateway_config::ProviderDialect,
) -> String {
    let kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() {
        "request"
    } else {
        "network"
    };
    if dialect == crate::gateway_config::ProviderDialect::OpenAiChatCompletions {
        format!(
            "configured provider request failed ({kind}); check its connection and request budget"
        )
    } else {
        format!("provider request failed ({kind})")
    }
}

/// Attach the usage a provider reported to the attempt that carried it.
///
/// Usage is only known after the response body has been read, which happens in
/// the caller, so it lands as a receipts update on the already-settled attempt
/// rather than as part of settlement.
pub(crate) fn record_usage(
    call: &AdmittedCall,
    attempt_id: &BoundedId,
    input_tokens: u64,
    output_tokens: u64,
) -> Result<()> {
    let Some(usage) = attempt_binding::usage_receipt(input_tokens, output_tokens) else {
        return Ok(());
    };
    call.attach_usage(attempt_id, usage)
}
