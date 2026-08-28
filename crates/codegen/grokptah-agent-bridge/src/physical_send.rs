//! Task-local binding of one physical provider request to its durable attempt.
//!
//! [`crate::attempt_binding`] records *what* an attempt is bound to. This
//! module is the other half: it is what the HTTP client actually reaches for
//! while a request is on the wire, so the durable record advances from real
//! transport outcomes rather than from an optimistic guess made beside them.
//!
//! A caller scopes a binding around a turn; the turn may or may not reach a
//! provider. [`mark_sending`] is what crosses the send boundary, at the one
//! instant there is genuinely a request to put on a socket, so a turn that
//! never dispatches — a slash command, an unresolvable credential — is never
//! recorded as one that did.
//!
//! # Two separate questions
//!
//! Whether an attempt is bound and whether its key may go on the wire are
//! different facts, and conflating them is a duplicate-charge bug in both
//! directions:
//!
//! * [`is_bound`] answers *may this host quietly re-send?* It is true for
//!   every bound attempt, on every dialect. An unbound send has no durable
//!   record to reconcile against, so it must not be repeated either.
//! * [`wire_idempotency_key`] answers *will the provider recognise a
//!   duplicate?* It is `Some` only where the dialect contract defines the
//!   header (see [`RequestDialect::permits_idempotency_key`]).
//!
//! Reading the second where the first is meant would let a compatible gateway
//! — which promises nothing about idempotency — be treated as unbound and
//! silently retried.

use std::future::Future;

use grokptah_agent_sdk::attempt::{BoundedId, SendState};
use grokptah_agent_sdk::launch::RequestDialect;

use crate::orchestration::OrchStore;

/// The response header a provider uses to identify one request.
///
/// Read-only, and only ever recorded after [`BoundedId`] accepts it, so a
/// hostile or malformed provider header cannot reach a durable record.
const PROVIDER_REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Clone)]
pub struct PhysicalSendBinding {
    pub store: OrchStore,
    pub attempt_id: String,
    /// The idempotency key recorded on this attempt.
    ///
    /// Always present in the durable record. Whether it is also carried on
    /// the wire is decided separately, by `wire_key_permitted`.
    pub idempotency_key: String,
    /// Whether the bound route's dialect contract defines the key on the wire.
    pub wire_key_permitted: bool,
}

impl PhysicalSendBinding {
    /// Bind one durable attempt to the request this task is about to make.
    ///
    /// The wire decision is taken here, once, from the dialect the attempt was
    /// *recorded* under — never from live configuration — so the header that
    /// goes out and the record that explains it cannot disagree.
    pub fn new(
        store: OrchStore,
        attempt_id: String,
        idempotency_key: String,
        dialect: RequestDialect,
    ) -> Self {
        Self {
            store,
            attempt_id,
            idempotency_key,
            wire_key_permitted: dialect.permits_idempotency_key(),
        }
    }
}

tokio::task_local! {
    static PHYSICAL_SEND: PhysicalSendBinding;
}

pub async fn scope_optional<F: Future>(binding: Option<PhysicalSendBinding>, fut: F) -> F::Output {
    match binding {
        Some(binding) => PHYSICAL_SEND.scope(binding, fut).await,
        None => fut.await,
    }
}

/// Whether a durable attempt is bound to this task.
///
/// The guard on every automatic re-send inside the HTTP client. Those guards
/// sit after a dispatch has already been made, so a bound attempt there has
/// crossed the send boundary and repeating it would duplicate work the
/// provider may already have performed. Reconciliation is an operator
/// decision made against the recorded attempt, not a retry loop.
pub fn is_bound() -> bool {
    PHYSICAL_SEND.try_with(|_| ()).is_ok()
}

/// The idempotency key to put on the wire, when the contract defines one.
///
/// `None` both when nothing is bound and when the bound dialect publishes no
/// idempotency contract. In the second case the host still refuses to
/// auto-retry (see [`is_bound`]); it simply does not claim a provider-side
/// guarantee it cannot obtain.
pub fn wire_idempotency_key() -> Option<String> {
    PHYSICAL_SEND
        .try_with(|binding| {
            binding
                .wire_key_permitted
                .then(|| binding.idempotency_key.clone())
        })
        .ok()
        .flatten()
}

/// About to put this request on a socket.
///
/// The send boundary, crossed here and nowhere else. The durable write
/// completes before the bytes move, so a process that dies at any instant
/// after this leaves `sending` on disk — deliberately not auto-retryable,
/// because an interrupted send is indistinguishable from a delivered one
/// without asking the provider.
///
/// Idempotent across the rounds of one turn: a tool loop that calls the
/// provider repeatedly is one attempt, and every call after the first finds
/// the boundary already crossed.
pub fn mark_sending() {
    advance(SendState::Sending, |_| {});
}

/// A response reached this host, so the request provably left it.
///
/// `provider_request_id` is the provider's own identifier taken from the
/// response headers when it published one. Nothing else is ever written into
/// [`ProviderReceipts::request`]: this host's idempotency key is not a
/// provider receipt, and recording it as one would manufacture evidence that
/// the provider had identified a request it may never have named.
pub fn mark_sent(provider_request_id: Option<&str>) {
    let receipt = provider_request_id.and_then(BoundedId::new);
    advance(SendState::Sent, move |attempt| {
        if attempt.receipts.request.is_none() {
            attempt.receipts.request = receipt.clone();
        }
        // A status line from the provider is an acknowledgement of receipt
        // even when it publishes no identifier — the case
        // `ProviderReceipts::provider_replied` exists to name.
        attempt.receipts.provider_replied = true;
    });
}

/// The response body has begun to arrive.
pub fn mark_responding() {
    advance(SendState::Responding, |attempt| {
        attempt.receipts.provider_replied = true;
    });
}

/// The request left this host and no reply ever came back.
///
/// The one moment `uncertain` is reachable: the lattice allows it only from
/// `sending`, because once a response has been seen the request demonstrably
/// arrived and the open question becomes completion, not delivery. A failure
/// to *connect* is deliberately left alone — the attempt stays `sending`,
/// which is equally non-retryable, rather than asserting an ambiguity that was
/// never reached.
///
/// Recording it here rather than at turn end is what makes the ambiguity
/// survive a crash in the window between the two.
pub fn mark_uncertain() {
    advance(SendState::Uncertain, |_| {});
}

fn advance<F>(next: SendState, receipts: F)
where
    F: Fn(&mut grokptah_agent_sdk::attempt::ProviderAttempt),
{
    let _ = PHYSICAL_SEND.try_with(|binding| {
        let _ = binding
            .store
            .update_attempt(&binding.attempt_id, |attempt| {
                if attempt.send_state == next {
                    return Ok(());
                }
                // Receipts are recorded before the transition so the record is
                // never momentarily a `sent` state with nothing behind it.
                let before = attempt.receipts.clone();
                receipts(attempt);
                if attempt.send_state.permits_transition_to(next) {
                    attempt.advance(next).map_err(anyhow::Error::msg)?;
                } else {
                    // The lattice refused the move, so the receipts that were
                    // only justified by it are rolled back with it.
                    attempt.receipts = before;
                }
                Ok(())
            });
    });
}

/// The provider's own request identifier from a response, when it published
/// one.
pub fn provider_request_id_from(headers: &reqwest::header::HeaderMap) -> Option<&str> {
    headers
        .get(PROVIDER_REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
}
