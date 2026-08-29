//! The physical provider-send chokepoint (#478).
//!
//! This is the only module in the crate that constructs an inference HTTP
//! client or a `/chat/completions` URL. Everything that can reach a model goes
//! through [`dispatch`], and [`dispatch`] cannot be called without an
//! [`AttemptHandle`]-producing [`ProviderSendContext`], so there is no shape of
//! "provider call site that forgot to bind".
//!
//! The `reqwest::Response` never escapes this module: callers read it through
//! [`ResponseReader`], which records what the transport actually observed.
//! A reader dropped mid-response marks its attempt `Uncertain` rather than
//! leaving a delivered request looking un-sent.

use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use super::crash::{checkpoint, CrashCut, CutFired};
use super::dialect::WireDialect;
use super::identity::{RequestDigest, RouteIncarnation};
use super::ledger::{AttemptHandle, LedgerError};
use super::record::{
    AccountingRecord, AuditOutcome, CancellationRecord, ReceiptRecord, Settlement,
    SettlementOutcome,
};
use super::state::{
    DeliveryKnowledge, HostEvidence, HostFailureClass, ProviderAttemptState, TransportEvidence,
    UncertaintyClass,
};
use super::ProviderSendContext;

/// Connect timeout for every inference request. Matches the value the desktop
/// transport used before the chokepoint existed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// What the caller wants back from the provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseAccept {
    /// `application/json`.
    Json,
    /// `text/event-stream`, with a JSON fallback the provider may still choose.
    EventStream,
}

impl ResponseAccept {
    fn header_value(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::EventStream => "text/event-stream",
        }
    }
}

/// The one definition of the provider completions route.
///
/// Both the request the chokepoint sends and any route *identity* derived for
/// diagnostics come from here, so the URL shape is not knowledge that can drift
/// between two places — and the structural gate can hold "constructed in one
/// module" as a literal truth.
pub(crate) fn completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// One physical attempt as a diagnostics or certification recorder sees it.
///
/// Derived from the durable lattice record at the attempt's terminal point, so
/// the recorder is a *projection* of the one truth rather than a second ledger
/// tracking the same sends independently. It is emitted exactly once per
/// physical attempt, including on the paths that never reached the wire.
#[derive(Debug)]
pub struct ObservedAttempt {
    pub state: ProviderAttemptState,
    pub outcome: Option<SettlementOutcome>,
    pub status: Option<u16>,
    pub headers: Option<reqwest::header::HeaderMap>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub uncertainty: Option<UncertaintyClass>,
    /// The response arrived but the host could not use it.
    pub protocol_error: bool,
}

/// Receives one [`ObservedAttempt`] per physical attempt.
pub type ObservationSink = std::sync::Arc<dyn Fn(&ObservedAttempt) + Send + Sync>;

/// One concrete provider request, described rather than constructed.
///
/// The caller supplies the body and route material; this module turns them into
/// an actual HTTP request. That split is what the structural gate enforces.
pub struct ProviderRequestSpec<'a> {
    /// `None` for a provider the operator configured without a credential. An
    /// unauthenticated request is still a physical send and is still bound.
    pub credentials: Option<&'a crate::auth_store::WireCredentials>,
    /// Provider base URL. Hashed into the binding; never stored in the clear.
    pub base_url: &'a str,
    pub wire_model: &'a str,
    pub dialect: WireDialect,
    /// Non-secret identifier of the credential incarnation in force.
    pub credential_binding: Option<&'a str>,
    pub body: &'a serde_json::Value,
    pub accept: ResponseAccept,
    /// `x-grok-effort`, for the dialects that take it.
    pub effort_header: Option<&'a str>,
    pub request_timeout: Duration,
    /// Optional projection sink. Never a second ledger: it is fed from the
    /// attempt's own durable state.
    pub observation: Option<ObservationSink>,
}

impl ProviderRequestSpec<'_> {
    fn route_incarnation(&self) -> RouteIncarnation {
        RouteIncarnation::new(
            self.base_url,
            self.wire_model,
            self.dialect,
            self.credentials
                .map(|credentials| credentials.method.as_str())
                .unwrap_or("unauthenticated"),
            self.credential_binding,
        )
    }
}

/// Coarse classification of what the transport observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportOutcome {
    /// The connection was never established: proven un-sent.
    NotSent,
    /// Response headers arrived.
    Acknowledged,
    /// A write may have happened and nothing more is known.
    Uncertain(UncertaintyClass),
}

/// A send failure that carries what is known about delivery.
///
/// Callers implement retry stand-down from `may_auto_retry`, never from the
/// message text, so a re-worded error can never change retry behaviour.
#[derive(Debug)]
pub struct ProviderSendError {
    message: String,
    delivery: DeliveryKnowledge,
    may_auto_retry: bool,
    uncertainty: Option<UncertaintyClass>,
}

impl ProviderSendError {
    fn new(
        message: impl Into<String>,
        delivery: DeliveryKnowledge,
        may_auto_retry: bool,
        uncertainty: Option<UncertaintyClass>,
    ) -> Self {
        Self {
            message: message.into(),
            delivery,
            may_auto_retry,
            uncertainty,
        }
    }

    /// A failure that never reached the wire.
    pub fn not_sent(message: impl Into<String>) -> Self {
        Self::new(message, DeliveryKnowledge::KnownNotDelivered, true, None)
    }

    /// A failure after a possible write. Never auto-retryable.
    pub fn uncertain(message: impl Into<String>, class: UncertaintyClass) -> Self {
        Self::new(message, DeliveryKnowledge::Unknown, false, Some(class))
    }

    /// A host-side failure that cannot be attributed to the wire at all.
    pub fn host(message: impl Into<String>) -> Self {
        Self::new(message, DeliveryKnowledge::KnownNotDelivered, false, None)
    }

    pub fn delivery(&self) -> DeliveryKnowledge {
        self.delivery
    }

    /// The single retry rule, as seen by a caller.
    pub fn may_auto_retry(&self) -> bool {
        self.may_auto_retry
    }

    pub fn uncertainty(&self) -> Option<UncertaintyClass> {
        self.uncertainty
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ProviderSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if self.delivery == DeliveryKnowledge::Unknown {
            f.write_str(" (delivery unknown; not retried automatically)")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderSendError {}

impl From<LedgerError> for ProviderSendError {
    fn from(value: LedgerError) -> Self {
        match value {
            LedgerError::ScopeNotSettled { state, .. } => Self::new(
                format!(
                    "a previous provider send in this scope is unresolved ({state}); \
                     it must be resolved before another request is admitted"
                ),
                state.delivery_knowledge(),
                false,
                None,
            ),
            LedgerError::Interrupted(cut) => Self::host(cut.to_string()),
            other => Self::host(other.to_string()),
        }
    }
}

impl From<CutFired> for ProviderSendError {
    fn from(value: CutFired) -> Self {
        Self::host(value.to_string())
    }
}

/// Classify a `reqwest` send error into transport evidence.
///
/// The only classification that yields `NotSent` is a connection that was never
/// established. A timeout is *not* evidence of non-delivery: `reqwest`'s total
/// timeout covers the whole exchange, so a timed-out request may well have been
/// received and acted upon.
fn classify_send_error(error: &reqwest::Error) -> TransportOutcome {
    if error.is_connect() && !error.is_timeout() {
        return TransportOutcome::NotSent;
    }
    if error.is_timeout() {
        return TransportOutcome::Uncertain(UncertaintyClass::Timeout);
    }
    if error.is_redirect() {
        // Redirects are disabled on the client, so this means the provider tried
        // to move us; the original request was certainly delivered.
        return TransportOutcome::Uncertain(UncertaintyClass::TransportError);
    }
    TransportOutcome::Uncertain(UncertaintyClass::TransportError)
}

fn classify_body_error(error: &reqwest::Error) -> UncertaintyClass {
    if error.is_timeout() {
        UncertaintyClass::Timeout
    } else if error.is_decode() {
        UncertaintyClass::ResponseParse
    } else {
        UncertaintyClass::ConnectionReset
    }
}

/// Send one bound provider request.
///
/// Ordering, which is the whole contract:
/// 1. digest the body,
/// 2. persist `Preparing` (this also runs the scope's admission rule),
/// 3. build the client and URL,
/// 4. persist `Sending` and fsync it,
/// 5. only then create the send future.
pub async fn dispatch(
    context: &ProviderSendContext,
    spec: ProviderRequestSpec<'_>,
    cancel: &CancellationToken,
) -> Result<SentRequest, ProviderSendError> {
    let body_bytes = serde_json::to_vec(spec.body)
        .map_err(|error| ProviderSendError::host(format!("provider request encoding: {error}")))?;
    let request_bytes = body_bytes.len() as u64;
    let digest = RequestDigest::of_body(&body_bytes);

    let sink = spec.observation.clone();
    let emit = |state: ProviderAttemptState,
                outcome: Option<SettlementOutcome>,
                status: Option<u16>,
                uncertainty: Option<UncertaintyClass>| {
        if let Some(sink) = sink.as_ref() {
            sink(&ObservedAttempt {
                state,
                outcome,
                status,
                headers: None,
                request_bytes,
                response_bytes: 0,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                uncertainty,
                protocol_error: false,
            });
        }
    };

    // (2) Durable intent before admission.
    let mut handle = match context.begin_attempt(spec.route_incarnation(), digest) {
        Ok(handle) => handle,
        Err(error) => {
            // Admission refusal is still a physical-send decision worth
            // projecting: the recorder should see that nothing was sent.
            emit(
                ProviderAttemptState::NotSent,
                Some(SettlementOutcome::NotSent),
                None,
                None,
            );
            return Err(error.into());
        }
    };

    // Everything from here to mark_sending is still provably pre-wire, so a
    // failure resolves to NotSent on host evidence rather than to uncertainty.
    let pre_wire = |handle: &mut AttemptHandle,
                    detail: HostFailureClass,
                    message: String|
     -> ProviderSendError {
        let _ = context
            .ledger()
            .mark_not_sent(handle, HostEvidence::OwnerObservedBeforeDispatch { detail });
        emit(
            ProviderAttemptState::NotSent,
            Some(SettlementOutcome::NotSent),
            None,
            None,
        );
        ProviderSendError::not_sent(message)
    };

    if cancel.is_cancelled() {
        return Err(pre_wire(
            &mut handle,
            HostFailureClass::CancelledBeforeDispatch,
            "cancelled".into(),
        ));
    }

    // (3) The one inference client in the crate.
    let client = match reqwest::Client::builder()
        .timeout(spec.request_timeout)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!(
            "grok/{} (GrokPtah)",
            crate::auth_store::client_version_header()
        ))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return Err(pre_wire(
                &mut handle,
                HostFailureClass::ClientConstruction,
                format!("provider client could not be built: {error}"),
            ));
        }
    };

    // (3b) The one place a completions URL is built.
    let url = completions_url(spec.base_url);

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", spec.accept.header_value());
    if let Some(effort) = spec.effort_header {
        request = request.header("x-grok-effort", effort);
    }
    // Host idempotency identity goes on the wire only for a dialect that has
    // explicitly declared support. None currently has, so none currently gets
    // one — and the host key stays usable regardless, because it exists for the
    // host's own recognition, not for the provider's.
    if let Some((name, value)) = spec
        .dialect
        .idempotency_support()
        .header_for(handle.binding().host_idempotency())
    {
        request = request.header(name, value);
    }
    let request = match spec.credentials {
        Some(credentials) => {
            crate::auth_store::apply_auth_headers(request, credentials, spec.base_url)
        }
        None => request,
    };
    let request = request.body(body_bytes);

    if let Err(cut) = checkpoint(CrashCut::MidWrite) {
        // A cut here is exactly the "died while writing" case: `Sending` is not
        // durable yet, so the record is still `Preparing` and recovery will
        // prove non-delivery. Mark it explicitly for the in-process case.
        return Err(pre_wire(
            &mut handle,
            HostFailureClass::InjectedCut,
            cut.to_string(),
        ));
    }

    // (4) Durable, fsynced, immediately before any byte can move.
    context.ledger().mark_sending(&mut handle)?;

    // (5) From this line on the host cannot prove non-delivery by itself.
    let ledger = context.ledger().clone();
    let sent = tokio::select! {
        result = request.send() => result,
        _ = cancel.cancelled() => {
            let _ = ledger.apply_transport(
                &mut handle,
                TransportEvidence::PossibleWriteUnresolved {
                    class: UncertaintyClass::CancelledAfterDispatch,
                },
            );
            emit(
                ProviderAttemptState::Uncertain,
                Some(SettlementOutcome::Uncertain),
                None,
                Some(UncertaintyClass::CancelledAfterDispatch),
            );
            return Err(ProviderSendError::uncertain(
                "cancelled after the request was dispatched",
                UncertaintyClass::CancelledAfterDispatch,
            ));
        }
    };

    let response = match sent {
        Ok(response) => response,
        Err(error) => {
            return Err(match classify_send_error(&error) {
                TransportOutcome::NotSent => {
                    let _ = ledger.apply_transport(
                        &mut handle,
                        TransportEvidence::ConnectionNeverEstablished,
                    );
                    emit(
                        ProviderAttemptState::NotSent,
                        Some(SettlementOutcome::NotSent),
                        None,
                        None,
                    );
                    ProviderSendError::not_sent("provider could not be connected")
                }
                TransportOutcome::Uncertain(class) => {
                    let _ = ledger.apply_transport(
                        &mut handle,
                        TransportEvidence::PossibleWriteUnresolved { class },
                    );
                    emit(
                        ProviderAttemptState::Uncertain,
                        Some(SettlementOutcome::Uncertain),
                        None,
                        Some(class),
                    );
                    ProviderSendError::uncertain(
                        "provider request failed after it may have been written",
                        class,
                    )
                }
                TransportOutcome::Acknowledged => unreachable!("send error is never an ack"),
            });
        }
    };

    if let Err(cut) = checkpoint(CrashCut::AfterBytesNoHeaders) {
        let _ = ledger.apply_transport(
            &mut handle,
            TransportEvidence::PossibleWriteUnresolved {
                class: UncertaintyClass::ProcessInterrupted,
            },
        );
        emit(
            ProviderAttemptState::Uncertain,
            Some(SettlementOutcome::Uncertain),
            None,
            Some(UncertaintyClass::ProcessInterrupted),
        );
        return Err(ProviderSendError::from(cut));
    }

    let status = response.status().as_u16();
    ledger.apply_transport(&mut handle, TransportEvidence::ResponseHeaders { status })?;

    if let Err(cut) = checkpoint(CrashCut::AfterHeaders) {
        let _ = ledger.apply_transport(
            &mut handle,
            TransportEvidence::PossibleWriteUnresolved {
                class: UncertaintyClass::ProcessInterrupted,
            },
        );
        emit(
            ProviderAttemptState::Uncertain,
            Some(SettlementOutcome::Uncertain),
            Some(status),
            Some(UncertaintyClass::ProcessInterrupted),
        );
        return Err(ProviderSendError::from(cut));
    }

    Ok(SentRequest {
        response,
        handle,
        context: context.clone(),
        request_bytes,
        observation: spec.observation,
    })
}

/// A dispatched request whose headers have arrived.
pub struct SentRequest {
    response: reqwest::Response,
    handle: AttemptHandle,
    context: ProviderSendContext,
    request_bytes: u64,
    observation: Option<ObservationSink>,
}

// A redacted Debug: enough to identify the attempt in a test failure, never
// the response body, the route, or the credential.
impl std::fmt::Debug for SentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SentRequest")
            .field("ordinal", &self.handle.ordinal())
            .field("state", &self.handle.state())
            .field("status", &self.response.status().as_u16())
            .finish()
    }
}

impl SentRequest {
    pub fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub fn headers(&self) -> &reqwest::header::HeaderMap {
        self.response.headers()
    }

    pub fn content_type(&self) -> String {
        self.response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    pub fn attempt(&self) -> &AttemptHandle {
        &self.handle
    }

    /// Take the response body under lattice observation.
    pub fn into_reader(self) -> ResponseReader {
        let status = self.response.status().as_u16();
        let headers = self.response.headers().clone();
        ResponseReader {
            stream: self.response.bytes_stream().boxed(),
            handle: self.handle,
            context: self.context,
            request_bytes: self.request_bytes,
            response_bytes: 0,
            status,
            headers,
            stream_complete: false,
            finished: false,
            settled: false,
            observation: self.observation,
            emitted: false,
        }
    }
}

/// Reads a response body while keeping the durable lattice honest.
///
/// Every byte observed moves the attempt to `Responding`; a clean end of stream
/// moves it to `Settled`; anything else moves it to `Uncertain`. Dropping the
/// reader without settling marks the attempt `Uncertain` too, so a caller that
/// returns early on a parse error cannot leave a delivered request recorded as
/// anything better than unknown.
pub struct ResponseReader {
    stream: futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    handle: AttemptHandle,
    context: ProviderSendContext,
    request_bytes: u64,
    response_bytes: u64,
    status: u16,
    headers: reqwest::header::HeaderMap,
    /// The body reached a clean end of stream.
    stream_complete: bool,
    finished: bool,
    settled: bool,
    observation: Option<ObservationSink>,
    emitted: bool,
}

impl ResponseReader {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn request_bytes(&self) -> u64 {
        self.request_bytes
    }

    pub fn response_bytes(&self) -> u64 {
        self.response_bytes
    }

    /// Whether the body reached a clean end of stream.
    pub fn stream_complete(&self) -> bool {
        self.stream_complete
    }

    pub fn attempt(&self) -> &AttemptHandle {
        &self.handle
    }

    pub fn state(&self) -> ProviderAttemptState {
        self.handle.state()
    }

    pub fn headers(&self) -> &reqwest::header::HeaderMap {
        &self.headers
    }

    /// Project this attempt exactly once, from its own durable state.
    fn emit(
        &mut self,
        outcome: Option<SettlementOutcome>,
        usage: Option<(Option<u64>, Option<u64>, Option<u64>)>,
        uncertainty: Option<UncertaintyClass>,
        protocol_error: bool,
    ) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        let Some(sink) = self.observation.clone() else {
            return;
        };
        let (prompt_tokens, completion_tokens, total_tokens) = usage.unwrap_or((None, None, None));
        sink(&ObservedAttempt {
            state: self.handle.state(),
            outcome,
            status: Some(self.status),
            headers: Some(self.headers.clone()),
            request_bytes: self.request_bytes,
            response_bytes: self.response_bytes,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            uncertainty,
            protocol_error,
        });
    }

    /// Next body chunk, or `None` at a clean end of stream.
    pub async fn next_chunk(
        &mut self,
        cancel: &CancellationToken,
    ) -> Option<Result<bytes::Bytes, ProviderSendError>> {
        if self.finished {
            return None;
        }
        let chunk = tokio::select! {
            chunk = self.stream.next() => chunk,
            _ = cancel.cancelled() => {
                self.finished = true;
                self.mark_uncertain(UncertaintyClass::CancelledAfterDispatch);
                return Some(Err(ProviderSendError::uncertain(
                    "cancelled while reading the provider response",
                    UncertaintyClass::CancelledAfterDispatch,
                )));
            }
        };
        match chunk {
            None => {
                self.finished = true;
                // A clean end of stream is transport evidence that the *bytes*
                // finished. It is not evidence that the exchange settled: the
                // host still has to make sense of the payload, and a response it
                // cannot read tells it nothing about what the provider did. The
                // terminal state therefore comes from the settlement below, not
                // from here.
                self.stream_complete = true;
                None
            }
            Some(Ok(bytes)) => {
                self.response_bytes = self.response_bytes.saturating_add(bytes.len() as u64);
                let _ = self.context.ledger().apply_transport(
                    &mut self.handle,
                    TransportEvidence::ResponseBytes {
                        status: self.status,
                        bytes: self.response_bytes,
                    },
                );
                if let Err(cut) = checkpoint(CrashCut::MidStream) {
                    self.finished = true;
                    self.mark_uncertain(UncertaintyClass::ProcessInterrupted);
                    return Some(Err(ProviderSendError::from(cut)));
                }
                Some(Ok(bytes))
            }
            Some(Err(error)) => {
                self.finished = true;
                let class = classify_body_error(&error);
                self.mark_uncertain(class);
                Some(Err(ProviderSendError::uncertain(
                    "provider response body ended before it was complete",
                    class,
                )))
            }
        }
    }

    /// Read the whole body under the shared bounded accumulator.
    pub async fn read_to_string(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<String, ProviderSendError> {
        let mut body = crate::sse::BoundedBodyAccumulator::new();
        while let Some(chunk) = self.next_chunk(cancel).await {
            let chunk = chunk?;
            body.push(&chunk).map_err(|error| {
                self.mark_uncertain(UncertaintyClass::ResponseParse);
                ProviderSendError::uncertain(
                    format!("provider response body rejected: {error}"),
                    UncertaintyClass::ResponseParse,
                )
            })?;
        }
        if let Err(cut) = checkpoint(CrashCut::AfterBody) {
            self.mark_uncertain(UncertaintyClass::ProcessInterrupted);
            return Err(ProviderSendError::from(cut));
        }
        body.finish().map_err(|error| {
            ProviderSendError::uncertain(
                format!("provider response body rejected: {error}"),
                UncertaintyClass::ResponseParse,
            )
        })
    }

    fn mark_uncertain(&mut self, class: UncertaintyClass) {
        let _ = self.context.ledger().apply_transport(
            &mut self.handle,
            TransportEvidence::PossibleWriteUnresolved { class },
        );
    }

    /// Settle a successful exchange.
    pub fn settle_completed(
        &mut self,
        provider_receipt: Option<&str>,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Result<(), ProviderSendError> {
        self.settle(Settlement {
            outcome: SettlementOutcome::Completed,
            cancellation: CancellationRecord::NotRequested,
            receipt: ReceiptRecord {
                provider_receipt: provider_receipt.map(|value| {
                    // The provider's id is the provider's, and is stored opaque
                    // so a projection can say "there was a receipt" without
                    // ever handing one out.
                    super::identity::opaque_digest("grokptah.provider_send.receipt.v1", &[value])
                }),
                status: Some(self.status),
            },
            accounting: AccountingRecord {
                prompt_tokens,
                completion_tokens,
                request_bytes: self.request_bytes,
                response_bytes: self.response_bytes,
            },
            audit: AuditOutcome::Accounted,
            settled_at: Utc::now(),
            uncertainty: None,
        })?;
        self.emit(
            Some(SettlementOutcome::Completed),
            Some((prompt_tokens, completion_tokens, total_tokens)),
            None,
            false,
        );
        Ok(())
    }

    /// Settle an exchange the provider itself rejected.
    pub fn settle_rejected(&mut self) -> Result<(), ProviderSendError> {
        self.settle(Settlement {
            outcome: SettlementOutcome::ProviderRejected,
            cancellation: CancellationRecord::NotRequested,
            receipt: ReceiptRecord {
                provider_receipt: None,
                status: Some(self.status),
            },
            accounting: AccountingRecord {
                request_bytes: self.request_bytes,
                response_bytes: self.response_bytes,
                ..AccountingRecord::default()
            },
            audit: AuditOutcome::Accounted,
            settled_at: Utc::now(),
            uncertainty: None,
        })?;
        self.emit(Some(SettlementOutcome::ProviderRejected), None, None, false);
        Ok(())
    }

    /// Settle an exchange whose delivery outcome is unknown.
    ///
    /// Also the settlement for a response the host could not parse: the goal
    /// is delivery truth, and a response we cannot read tells us nothing about
    /// whether the provider acted on the request.
    pub fn settle_uncertain(&mut self, class: UncertaintyClass) -> Result<(), ProviderSendError> {
        self.settle(Settlement {
            outcome: SettlementOutcome::Uncertain,
            cancellation: CancellationRecord::NotRequested,
            receipt: ReceiptRecord::default(),
            accounting: AccountingRecord {
                request_bytes: self.request_bytes,
                response_bytes: self.response_bytes,
                ..AccountingRecord::default()
            },
            audit: AuditOutcome::Unresolved,
            settled_at: Utc::now(),
            uncertainty: Some(class),
        })?;
        self.emit(
            Some(SettlementOutcome::Uncertain),
            None,
            Some(class),
            class == UncertaintyClass::ResponseParse,
        );
        Ok(())
    }

    fn settle(&mut self, settlement: Settlement) -> Result<(), ProviderSendError> {
        self.context
            .ledger()
            .settle(&mut self.handle, settlement)
            .map_err(ProviderSendError::from)?;
        self.settled = true;
        Ok(())
    }
}

impl Drop for ResponseReader {
    fn drop(&mut self) {
        if self.settled || self.handle.state().is_terminal() {
            return;
        }
        // A reader abandoned mid-response leaves a delivered request whose
        // outcome nobody observed. That is uncertainty, and it is recorded as
        // such rather than being left to look like a clean failure. A body that
        // *did* complete but was never judged is uncertainty too — the host
        // simply never established what the provider produced.
        let class = if self.stream_complete {
            UncertaintyClass::ResponseParse
        } else {
            UncertaintyClass::UnexpectedEof
        };
        let _ = self.context.ledger().apply_transport(
            &mut self.handle,
            TransportEvidence::PossibleWriteUnresolved { class },
        );
        let _ = self.context.ledger().settle(
            &mut self.handle,
            Settlement {
                outcome: SettlementOutcome::Uncertain,
                cancellation: CancellationRecord::NotRequested,
                receipt: ReceiptRecord::default(),
                accounting: AccountingRecord {
                    request_bytes: self.request_bytes,
                    response_bytes: self.response_bytes,
                    ..AccountingRecord::default()
                },
                audit: AuditOutcome::Unresolved,
                settled_at: Utc::now(),
                uncertainty: Some(class),
            },
        );
        self.emit(Some(SettlementOutcome::Uncertain), None, Some(class), false);
    }
}
