//! Safe, opt-in structural observations for physical provider HTTP attempts.
//!
//! This module deliberately has no payload, URL, filesystem, or arbitrary-header
//! escape hatch. It is suitable for production-path diagnostics and bounded
//! certification recorders, but not for recording provider conversations.

use serde::ser::Serializer;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Instant;
use uuid::Uuid;

pub const MAX_ATTEMPT_NUMBER: u32 = 1_000_000;
pub const MAX_OBSERVED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_LATENCY_MILLIS: u64 = 24 * 60 * 60 * 1_000;
pub const MAX_USAGE_TOKENS: u64 = 100_000_000;
pub const MAX_RECORDER_CAPACITY: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceMode {
    Disabled,
    MetadataOnly,
    SyntheticPayloads,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRouteClass {
    GrokBuildProxy,
    XaiApi,
    CompatibleGateway,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDialect {
    GrokBuild,
    XaiChatCompletions,
    OpenaiCompatible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMethod {
    GrokBuildOidc,
    XaiApiKey,
    GatewayManaged,
    GatewayApiKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFraming {
    None,
    FixedLength,
    Chunked,
    ServerSentEvents,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptDisposition {
    Completed,
    HttpError,
    TransportError,
    Timeout,
    Cancelled,
    ProtocolError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseContentClass {
    Json,
    EventStream,
    Empty,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestHeaderName {
    Accept,
    ContentType,
    UserAgent,
    XAuthenticateResponse,
    XGrokClientMode,
    XGrokClientVersion,
    XGrokEffort,
    XXaiTokenAuth,
}

impl RequestHeaderName {
    pub fn parse(name: &str) -> Result<Self, ObservationError> {
        if name.eq_ignore_ascii_case("accept") {
            Ok(Self::Accept)
        } else if name.eq_ignore_ascii_case("content-type") {
            Ok(Self::ContentType)
        } else if name.eq_ignore_ascii_case("user-agent") {
            Ok(Self::UserAgent)
        } else if name.eq_ignore_ascii_case("x-authenticateresponse") {
            Ok(Self::XAuthenticateResponse)
        } else if name.eq_ignore_ascii_case("x-grok-client-mode") {
            Ok(Self::XGrokClientMode)
        } else if name.eq_ignore_ascii_case("x-grok-client-version") {
            Ok(Self::XGrokClientVersion)
        } else if name.eq_ignore_ascii_case("x-grok-effort") {
            Ok(Self::XGrokEffort)
        } else if name.eq_ignore_ascii_case("x-xai-token-auth") {
            Ok(Self::XXaiTokenAuth)
        } else {
            Err(ObservationError::HeaderNotAllowlisted)
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::ContentType => "content-type",
            Self::UserAgent => "user-agent",
            Self::XAuthenticateResponse => "x-authenticateresponse",
            Self::XGrokClientMode => "x-grok-client-mode",
            Self::XGrokClientVersion => "x-grok-client-version",
            Self::XGrokEffort => "x-grok-effort",
            Self::XXaiTokenAuth => "x-xai-token-auth",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpMethod {
    Post,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDisposition {
    Delivered,
    Dropped,
    Panicked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationAcceptance {
    Accepted,
    Dropped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationError {
    InvalidScopeIdentifier,
    InvalidPublicModelIdentifier,
    InvalidOpaqueIdentity,
    InvalidFingerprint,
    HeaderNotAllowlisted,
    DuplicateHeaderName,
    InvalidAttemptNumber,
    InvalidStatusCode,
    InvalidUsage,
    StructuralBoundExceeded,
    InconsistentProviderRoute,
    InconsistentResponseMetadata,
    RecorderCapacityExceeded,
    AttemptBudgetExceeded,
}

impl fmt::Display for ObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidScopeIdentifier => "invalid opaque observation scope identifier",
            Self::InvalidPublicModelIdentifier => "invalid public model identifier",
            Self::InvalidOpaqueIdentity => "invalid opaque identity",
            Self::InvalidFingerprint => "invalid endpoint fingerprint",
            Self::HeaderNotAllowlisted => "request header name is not allowlisted",
            Self::DuplicateHeaderName => "duplicate request header name",
            Self::InvalidAttemptNumber => "invalid provider attempt number",
            Self::InvalidStatusCode => "invalid HTTP status code",
            Self::InvalidUsage => "invalid authoritative usage counts",
            Self::StructuralBoundExceeded => "provider observation exceeds a structural bound",
            Self::InconsistentProviderRoute => "inconsistent provider route metadata",
            Self::InconsistentResponseMetadata => "inconsistent provider response metadata",
            Self::RecorderCapacityExceeded => "observation recorder capacity exceeds its bound",
            Self::AttemptBudgetExceeded => "observation attempt budget exceeds its bound",
        })
    }
}

impl std::error::Error for ObservationError {}

#[derive(Clone, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OpaqueScopeId(String);

impl OpaqueScopeId {
    /// Accepts UUID-shaped or similarly opaque lowercase-hex identifiers only.
    pub fn new(value: &str) -> Result<Self, ObservationError> {
        let byte_len = value.len();
        let hex_count = value.bytes().filter(u8::is_ascii_hexdigit).count();
        let valid = (16..=64).contains(&byte_len)
            && hex_count >= 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-')
            && !value.contains("--");
        if !valid {
            return Err(ObservationError::InvalidScopeIdentifier);
        }
        Ok(Self(value.to_owned()))
    }

    /// Derive a stable opaque scope from a durable identifier that is not
    /// itself safe to retain. Desktop Run IDs have a `desktop-` prefix, for
    /// example; hashing keeps the observation scope deterministic without
    /// widening the accepted wire grammar or storing the identifier.
    pub fn from_stable_input(value: &str) -> Self {
        Self(format!("{:x}", Sha256::digest(value.as_bytes())))
    }
}

impl fmt::Debug for OpaqueScopeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueScopeId([opaque])")
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct ObservationScope {
    campaign_id: OpaqueScopeId,
    run_id: OpaqueScopeId,
    lane_id: OpaqueScopeId,
}

impl ObservationScope {
    pub fn new(campaign_id: OpaqueScopeId, run_id: OpaqueScopeId, lane_id: OpaqueScopeId) -> Self {
        Self {
            campaign_id,
            run_id,
            lane_id,
        }
    }

    pub fn campaign_id(&self) -> &OpaqueScopeId {
        &self.campaign_id
    }

    pub fn run_id(&self) -> &OpaqueScopeId {
        &self.run_id
    }

    pub fn lane_id(&self) -> &OpaqueScopeId {
        &self.lane_id
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PublicModelId(String);

impl PublicModelId {
    pub fn new(value: &str) -> Result<Self, ObservationError> {
        let valid = (6..=80).contains(&value.len())
            && value.starts_with("grok-")
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'.' | b'_')
            });
        if !valid {
            return Err(ObservationError::InvalidPublicModelIdentifier);
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Debug for PublicModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicModelId([public])")
    }
}

#[derive(Clone, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct OpaqueIdentity(String);

impl OpaqueIdentity {
    pub fn new(value: &str) -> Result<Self, ObservationError> {
        if !is_opaque_hash(value) {
            return Err(ObservationError::InvalidOpaqueIdentity);
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Debug for OpaqueIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueIdentity([opaque])")
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EndpointFingerprint(String);

impl EndpointFingerprint {
    pub fn new(value: &str) -> Result<Self, ObservationError> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ObservationError::InvalidFingerprint);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl fmt::Debug for EndpointFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EndpointFingerprint([sha256])")
    }
}

fn is_opaque_hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("opaque-")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "identity_class",
    content = "model_id",
    rename_all = "snake_case"
)]
pub enum ModelIdentity {
    Public(PublicModelId),
    Opaque(OpaqueIdentity),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderIdentity {
    model: ModelIdentity,
    endpoint_fingerprint: EndpointFingerprint,
}

impl ProviderIdentity {
    pub fn public(model: PublicModelId, endpoint_fingerprint: EndpointFingerprint) -> Self {
        Self {
            model: ModelIdentity::Public(model),
            endpoint_fingerprint,
        }
    }

    pub fn opaque(model: OpaqueIdentity, endpoint_fingerprint: EndpointFingerprint) -> Self {
        Self {
            model: ModelIdentity::Opaque(model),
            endpoint_fingerprint,
        }
    }

    pub fn model(&self) -> &ModelIdentity {
        &self.model
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestRouteIdentity {
    PublicChatCompletions,
    Opaque(OpaqueIdentity),
}

impl Serialize for RequestRouteIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::PublicChatCompletions => serializer.serialize_str("/v1/chat/completions"),
            Self::Opaque(identity) => serializer.serialize_str(&identity.0),
        }
    }
}

impl RequestRouteIdentity {
    pub const fn public_chat_completions() -> Self {
        Self::PublicChatCompletions
    }

    pub fn opaque(route: OpaqueIdentity) -> Self {
        Self::Opaque(route)
    }

    pub const fn method(&self) -> HttpMethod {
        HttpMethod::Post
    }

    pub const fn public_path(&self) -> Option<&'static str> {
        match self {
            Self::PublicChatCompletions => Some("/v1/chat/completions"),
            Self::Opaque(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AuthoritativeUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

impl AuthoritativeUsage {
    pub fn new(
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    ) -> Result<Self, ObservationError> {
        let computed = input_tokens
            .checked_add(output_tokens)
            .ok_or(ObservationError::InvalidUsage)?;
        if computed != total_tokens || total_tokens > MAX_USAGE_TOKENS {
            return Err(ObservationError::InvalidUsage);
        }
        Ok(Self {
            input_tokens,
            output_tokens,
            total_tokens,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ResponseMetadata {
    status_code: Option<u16>,
    content_class: Option<ResponseContentClass>,
    framing: ResponseFraming,
    disposition: AttemptDisposition,
}

impl ResponseMetadata {
    pub fn new(
        status_code: Option<u16>,
        content_class: Option<ResponseContentClass>,
        framing: ResponseFraming,
        disposition: AttemptDisposition,
    ) -> Result<Self, ObservationError> {
        if status_code.is_some_and(|status| !(100..=599).contains(&status)) {
            return Err(ObservationError::InvalidStatusCode);
        }
        let requires_status = matches!(
            disposition,
            AttemptDisposition::Completed
                | AttemptDisposition::HttpError
                | AttemptDisposition::ProtocolError
        );
        if requires_status != status_code.is_some()
            || matches!(content_class, Some(ResponseContentClass::EventStream))
                != matches!(framing, ResponseFraming::ServerSentEvents)
        {
            return Err(ObservationError::InconsistentResponseMetadata);
        }
        Ok(Self {
            status_code,
            content_class,
            framing,
            disposition,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AttemptMetrics {
    request_bytes: u64,
    response_bytes: u64,
    latency_millis: u64,
    usage: Option<AuthoritativeUsage>,
}

impl AttemptMetrics {
    pub fn new(
        request_bytes: u64,
        response_bytes: u64,
        latency_millis: u64,
        usage: Option<AuthoritativeUsage>,
    ) -> Result<Self, ObservationError> {
        if request_bytes > MAX_OBSERVED_BYTES
            || response_bytes > MAX_OBSERVED_BYTES
            || latency_millis > MAX_LATENCY_MILLIS
        {
            return Err(ObservationError::StructuralBoundExceeded);
        }
        Ok(Self {
            request_bytes,
            response_bytes,
            latency_millis,
            usage,
        })
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct ProviderObservation {
    evidence_mode: EvidenceMode,
    scope: ObservationScope,
    attempt_number: u32,
    route_class: ProviderRouteClass,
    dialect: ProviderDialect,
    credential_method: CredentialMethod,
    provider: ProviderIdentity,
    method: HttpMethod,
    route: RequestRouteIdentity,
    request_header_names: Vec<RequestHeaderName>,
    response: ResponseMetadata,
    metrics: AttemptMetrics,
}

impl ProviderObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        evidence_mode: EvidenceMode,
        scope: ObservationScope,
        attempt_number: u32,
        route_class: ProviderRouteClass,
        dialect: ProviderDialect,
        credential_method: CredentialMethod,
        provider: ProviderIdentity,
        route: RequestRouteIdentity,
        mut request_header_names: Vec<RequestHeaderName>,
        response: ResponseMetadata,
        metrics: AttemptMetrics,
    ) -> Result<Self, ObservationError> {
        if attempt_number == 0 || attempt_number > MAX_ATTEMPT_NUMBER {
            return Err(ObservationError::InvalidAttemptNumber);
        }
        request_header_names.sort_by_key(|name| name.as_str());
        if request_header_names
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(ObservationError::DuplicateHeaderName);
        }
        validate_route_consistency(route_class, dialect, credential_method, &provider, &route)?;
        Ok(Self {
            evidence_mode,
            scope,
            attempt_number,
            route_class,
            dialect,
            credential_method,
            provider,
            method: route.method(),
            route,
            request_header_names,
            response,
            metrics,
        })
    }

    pub const fn evidence_mode(&self) -> EvidenceMode {
        self.evidence_mode
    }

    pub fn scope(&self) -> &ObservationScope {
        &self.scope
    }

    pub const fn attempt_number(&self) -> u32 {
        self.attempt_number
    }
}

impl fmt::Debug for ProviderObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderObservation")
            .field("evidence_mode", &self.evidence_mode)
            .field("scope", &"[opaque]")
            .field("attempt_number", &self.attempt_number)
            .field("route_class", &self.route_class)
            .field("dialect", &self.dialect)
            .field("credential_method", &self.credential_method)
            .field("provider", &"[redacted]")
            .field("method", &self.method)
            .field("route", &"[redacted]")
            .field("request_header_names", &self.request_header_names)
            .field("response", &self.response)
            .field("metrics", &self.metrics)
            .finish()
    }
}

fn validate_route_consistency(
    route_class: ProviderRouteClass,
    dialect: ProviderDialect,
    credential_method: CredentialMethod,
    provider: &ProviderIdentity,
    route: &RequestRouteIdentity,
) -> Result<(), ObservationError> {
    let consistent = match route_class {
        ProviderRouteClass::GrokBuildProxy => {
            dialect == ProviderDialect::GrokBuild
                && credential_method == CredentialMethod::GrokBuildOidc
                && matches!(provider.model, ModelIdentity::Public(_))
                && matches!(route, RequestRouteIdentity::PublicChatCompletions)
        }
        ProviderRouteClass::XaiApi => {
            dialect == ProviderDialect::XaiChatCompletions
                && credential_method == CredentialMethod::XaiApiKey
                && matches!(provider.model, ModelIdentity::Public(_))
                && matches!(route, RequestRouteIdentity::PublicChatCompletions)
        }
        ProviderRouteClass::CompatibleGateway => {
            dialect == ProviderDialect::OpenaiCompatible
                && matches!(
                    credential_method,
                    CredentialMethod::GatewayManaged | CredentialMethod::GatewayApiKey
                )
                && matches!(provider.model, ModelIdentity::Opaque(_))
                && matches!(route, RequestRouteIdentity::Opaque(_))
        }
    };
    if consistent {
        Ok(())
    } else {
        Err(ObservationError::InconsistentProviderRoute)
    }
}

pub trait ProviderObservationSink: Send + Sync + 'static {
    fn observe(&self, observation: ProviderObservation) -> ObservationAcceptance;
}

#[derive(Clone)]
pub struct ProviderObserver {
    sink: Arc<dyn ProviderObservationSink>,
}

impl ProviderObserver {
    pub fn new(sink: Arc<dyn ProviderObservationSink>) -> Self {
        Self { sink }
    }

    pub fn notify(&self, observation: ProviderObservation) -> DeliveryDisposition {
        if observation.evidence_mode() == EvidenceMode::Disabled {
            return DeliveryDisposition::Dropped;
        }
        match catch_unwind(AssertUnwindSafe(|| self.sink.observe(observation))) {
            Ok(ObservationAcceptance::Accepted) => DeliveryDisposition::Delivered,
            Ok(ObservationAcceptance::Dropped) => DeliveryDisposition::Dropped,
            Err(_) => DeliveryDisposition::Panicked,
        }
    }
}

impl fmt::Debug for ProviderObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderObserver([sink])")
    }
}

/// Opt-in campaign-wide allocator for physical provider attempts. The
/// session owns only opaque scope IDs and a sink; it has no access to request
/// payloads, credentials, URLs, or host paths.
#[derive(Clone)]
pub struct ProviderObservationSession {
    mode: EvidenceMode,
    campaign_id: OpaqueScopeId,
    observer: ProviderObserver,
    next_attempt: Arc<AtomicU64>,
}

impl ProviderObservationSession {
    pub fn new(mode: EvidenceMode, campaign_id: OpaqueScopeId, observer: ProviderObserver) -> Self {
        Self {
            mode,
            campaign_id,
            observer,
            next_attempt: Arc::new(AtomicU64::new(0)),
        }
    }

    pub const fn mode(&self) -> EvidenceMode {
        self.mode
    }

    pub fn context(
        &self,
        run_id: &str,
        lane_id: Uuid,
    ) -> Result<ProviderObservationContext, ObservationError> {
        Ok(ProviderObservationContext {
            mode: self.mode,
            observer: self.observer.clone(),
            scope: ObservationScope::new(
                self.campaign_id.clone(),
                OpaqueScopeId::from_stable_input(run_id),
                OpaqueScopeId::new(&lane_id.to_string())?,
            ),
            next_attempt: self.next_attempt.clone(),
        })
    }
}

impl fmt::Debug for ProviderObservationSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderObservationSession")
            .field("mode", &self.mode)
            .field("campaign_id", &"[opaque]")
            .finish()
    }
}

/// A run/lane-scoped view over an observation session.
#[derive(Clone)]
pub struct ProviderObservationContext {
    mode: EvidenceMode,
    observer: ProviderObserver,
    scope: ObservationScope,
    next_attempt: Arc<AtomicU64>,
}

impl fmt::Debug for ProviderObservationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderObservationContext")
            .field("mode", &self.mode)
            .field("scope", &"[opaque]")
            .finish()
    }
}

impl ProviderObservationContext {
    pub fn begin_attempt(&self) -> Result<ProviderObservationAttempt, ObservationError> {
        let attempt_number = loop {
            let current = self.next_attempt.load(Ordering::Relaxed);
            if current >= MAX_ATTEMPT_NUMBER as u64 {
                return Err(ObservationError::AttemptBudgetExceeded);
            }
            if self
                .next_attempt
                .compare_exchange(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break (current + 1) as u32;
            }
        };
        Ok(ProviderObservationAttempt {
            mode: self.mode,
            observer: self.observer.clone(),
            scope: self.scope.clone(),
            attempt_number,
            started_at: Instant::now(),
        })
    }
}

/// One physical HTTP attempt. The caller supplies validated structural
/// metadata; this type supplies the shared scope, attempt number, and bounded
/// elapsed time.
pub struct ProviderObservationAttempt {
    mode: EvidenceMode,
    observer: ProviderObserver,
    scope: ObservationScope,
    attempt_number: u32,
    started_at: Instant,
}

impl ProviderObservationAttempt {
    pub const fn attempt_number(&self) -> u32 {
        self.attempt_number
    }

    pub fn scope(&self) -> &ObservationScope {
        &self.scope
    }

    pub const fn evidence_mode(&self) -> EvidenceMode {
        self.mode
    }

    pub fn elapsed_millis(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_millis()
            .min(MAX_LATENCY_MILLIS as u128) as u64
    }

    pub fn notify(&self, observation: ProviderObservation) -> DeliveryDisposition {
        if observation.evidence_mode() != self.mode
            || observation.attempt_number() != self.attempt_number
            || observation.scope() != &self.scope
        {
            return DeliveryDisposition::Dropped;
        }
        self.observer.notify(observation)
    }
}

#[derive(Clone)]
pub struct InMemoryObservationRecorder {
    inner: Arc<Mutex<RecorderState>>,
    dropped: Arc<AtomicU64>,
}

struct RecorderState {
    capacity: usize,
    observations: VecDeque<ProviderObservation>,
    attempt_numbers: HashSet<u32>,
}

impl InMemoryObservationRecorder {
    pub fn new(capacity: usize) -> Result<Self, ObservationError> {
        if capacity > MAX_RECORDER_CAPACITY {
            return Err(ObservationError::RecorderCapacityExceeded);
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(RecorderState {
                capacity,
                observations: VecDeque::with_capacity(capacity),
                attempt_numbers: HashSet::with_capacity(capacity),
            })),
            dropped: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn observer(&self) -> ProviderObserver {
        ProviderObserver::new(Arc::new(self.clone()))
    }

    pub fn snapshot(&self) -> Vec<ProviderObservation> {
        let mut observations: Vec<_> = self.lock().observations.iter().cloned().collect();
        observations.sort_by_key(ProviderObservation::attempt_number);
        observations
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.lock().observations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RecorderState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl fmt::Debug for InMemoryObservationRecorder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock();
        formatter
            .debug_struct("InMemoryObservationRecorder")
            .field("capacity", &state.capacity)
            .field("len", &state.observations.len())
            .field("dropped", &self.dropped_count())
            .finish()
    }
}

impl ProviderObservationSink for InMemoryObservationRecorder {
    fn observe(&self, observation: ProviderObservation) -> ObservationAcceptance {
        let mut state = match self.inner.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return ObservationAcceptance::Dropped;
            }
        };
        if state.observations.len() >= state.capacity
            || state
                .attempt_numbers
                .contains(&observation.attempt_number())
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return ObservationAcceptance::Dropped;
        }
        state.attempt_numbers.insert(observation.attempt_number());
        state.observations.push_back(observation);
        ObservationAcceptance::Accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const HASH_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn scope() -> ObservationScope {
        ObservationScope::new(
            OpaqueScopeId::new("018f1234-5678-7abc-8def-0123456789ab").unwrap(),
            OpaqueScopeId::new("018f1234-5678-7abc-8def-0123456789ac").unwrap(),
            OpaqueScopeId::new("018f1234-5678-7abc-8def-0123456789ad").unwrap(),
        )
    }

    fn observation(attempt_number: u32, mode: EvidenceMode) -> ProviderObservation {
        ProviderObservation::new(
            mode,
            scope(),
            attempt_number,
            ProviderRouteClass::GrokBuildProxy,
            ProviderDialect::GrokBuild,
            CredentialMethod::GrokBuildOidc,
            ProviderIdentity::public(
                PublicModelId::new("grok-code-fast-1").unwrap(),
                EndpointFingerprint::new(HASH_A).unwrap(),
            ),
            RequestRouteIdentity::public_chat_completions(),
            vec![RequestHeaderName::ContentType, RequestHeaderName::Accept],
            ResponseMetadata::new(
                Some(200),
                Some(ResponseContentClass::EventStream),
                ResponseFraming::ServerSentEvents,
                AttemptDisposition::Completed,
            )
            .unwrap(),
            AttemptMetrics::new(
                120,
                480,
                42,
                Some(AuthoritativeUsage::new(10, 20, 30).unwrap()),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn enums_serialize_as_stable_structural_values() {
        assert_eq!(
            serde_json::to_string(&EvidenceMode::MetadataOnly).unwrap(),
            r#""metadata_only""#
        );
        assert_eq!(
            serde_json::to_string(&ProviderRouteClass::GrokBuildProxy).unwrap(),
            r#""grok_build_proxy""#
        );
        assert_eq!(
            serde_json::to_string(&RequestHeaderName::ContentType).unwrap(),
            r#""content-type""#
        );
    }

    #[test]
    fn opaque_identifiers_reject_semantic_or_unbounded_values() {
        assert_eq!(
            OpaqueScopeId::new("alice-corporate-run").unwrap_err(),
            ObservationError::InvalidScopeIdentifier
        );
        assert_eq!(
            OpaqueIdentity::new(&format!("opaque-{}", HASH_A.to_uppercase())).unwrap_err(),
            ObservationError::InvalidOpaqueIdentity
        );
        assert!(OpaqueIdentity::new(&format!("opaque-{HASH_A}")).is_ok());
        assert!(EndpointFingerprint::new(HASH_B).is_ok());
        assert!(EndpointFingerprint::new("https://gateway.example").is_err());
    }

    #[test]
    fn durable_run_ids_are_hashed_into_opaque_context_scopes() {
        let recorder = InMemoryObservationRecorder::new(1).unwrap();
        let session = ProviderObservationSession::new(
            EvidenceMode::MetadataOnly,
            OpaqueScopeId::new("018f1234-5678-7abc-8def-0123456789ab").unwrap(),
            recorder.observer(),
        );
        let context = session
            .context("desktop-018f1234-5678-7abc-8def-0123456789ac", Uuid::nil())
            .unwrap();
        let attempt = context.begin_attempt().unwrap();
        let encoded = serde_json::to_string(attempt.scope().run_id()).unwrap();
        assert!(!encoded.contains("desktop-"));
        assert_eq!(encoded.len(), 66); // quoted 64-character SHA-256 digest
    }

    #[test]
    fn request_headers_are_a_tiny_positive_allowlist() {
        assert_eq!(
            RequestHeaderName::parse("CONTENT-TYPE").unwrap(),
            RequestHeaderName::ContentType
        );
        for forbidden in [
            "authorization",
            "cookie",
            "x-userid",
            "x-teamid",
            "x-request-id",
            "x-api-key",
            "x-custom-routing",
        ] {
            assert_eq!(
                RequestHeaderName::parse(forbidden).unwrap_err(),
                ObservationError::HeaderNotAllowlisted
            );
        }
    }

    #[test]
    fn provider_route_contracts_are_enforced() {
        let provider = ProviderIdentity::opaque(
            OpaqueIdentity::new(&format!("opaque-{HASH_B}")).unwrap(),
            EndpointFingerprint::new(HASH_C).unwrap(),
        );
        let result = ProviderObservation::new(
            EvidenceMode::MetadataOnly,
            scope(),
            1,
            ProviderRouteClass::CompatibleGateway,
            ProviderDialect::OpenaiCompatible,
            CredentialMethod::GatewayManaged,
            provider,
            RequestRouteIdentity::opaque(OpaqueIdentity::new(&format!("opaque-{HASH_C}")).unwrap()),
            vec![RequestHeaderName::Accept],
            ResponseMetadata::new(
                None,
                None,
                ResponseFraming::None,
                AttemptDisposition::TransportError,
            )
            .unwrap(),
            AttemptMetrics::new(1, 0, 4, None).unwrap(),
        );
        assert!(result.is_ok());

        let inconsistent = ProviderObservation::new(
            EvidenceMode::MetadataOnly,
            scope(),
            1,
            ProviderRouteClass::CompatibleGateway,
            ProviderDialect::XaiChatCompletions,
            CredentialMethod::GatewayManaged,
            ProviderIdentity::public(
                PublicModelId::new("grok-4").unwrap(),
                EndpointFingerprint::new(HASH_A).unwrap(),
            ),
            RequestRouteIdentity::public_chat_completions(),
            vec![],
            ResponseMetadata::new(
                None,
                None,
                ResponseFraming::None,
                AttemptDisposition::Timeout,
            )
            .unwrap(),
            AttemptMetrics::new(0, 0, 1, None).unwrap(),
        );
        assert_eq!(
            inconsistent.unwrap_err(),
            ObservationError::InconsistentProviderRoute
        );
    }

    #[test]
    fn bounds_usage_and_response_shape_are_enforced() {
        assert_eq!(
            AuthoritativeUsage::new(2, 3, 6).unwrap_err(),
            ObservationError::InvalidUsage
        );
        assert_eq!(
            AttemptMetrics::new(MAX_OBSERVED_BYTES + 1, 0, 0, None).unwrap_err(),
            ObservationError::StructuralBoundExceeded
        );
        assert_eq!(
            ResponseMetadata::new(
                Some(200),
                Some(ResponseContentClass::EventStream),
                ResponseFraming::Chunked,
                AttemptDisposition::Completed,
            )
            .unwrap_err(),
            ObservationError::InconsistentResponseMetadata
        );
    }

    #[test]
    fn recorder_accepts_out_of_order_completion_and_sorts_snapshot() {
        let recorder = InMemoryObservationRecorder::new(3).unwrap();
        let observer = recorder.observer();
        assert_eq!(
            observer.notify(observation(2, EvidenceMode::MetadataOnly)),
            DeliveryDisposition::Delivered
        );
        assert_eq!(
            observer.notify(observation(1, EvidenceMode::MetadataOnly)),
            DeliveryDisposition::Delivered
        );
        assert_eq!(
            observer.notify(observation(3, EvidenceMode::SyntheticPayloads)),
            DeliveryDisposition::Delivered
        );
        assert_eq!(
            observer.notify(observation(2, EvidenceMode::MetadataOnly)),
            DeliveryDisposition::Dropped
        );
        assert_eq!(recorder.len(), 3);
        assert_eq!(recorder.dropped_count(), 1);
        assert_eq!(
            recorder
                .snapshot()
                .iter()
                .map(ProviderObservation::attempt_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn recorder_counts_capacity_drops() {
        let recorder = InMemoryObservationRecorder::new(1).unwrap();
        let observer = recorder.observer();
        assert_eq!(
            observer.notify(observation(1, EvidenceMode::MetadataOnly)),
            DeliveryDisposition::Delivered
        );
        assert_eq!(
            observer.notify(observation(2, EvidenceMode::MetadataOnly)),
            DeliveryDisposition::Dropped
        );
        assert_eq!(recorder.dropped_count(), 1);
    }

    #[test]
    fn disabled_mode_never_reaches_the_sink() {
        let recorder = InMemoryObservationRecorder::new(1).unwrap();
        assert_eq!(
            recorder
                .observer()
                .notify(observation(1, EvidenceMode::Disabled)),
            DeliveryDisposition::Dropped
        );
        assert!(recorder.is_empty());
        assert_eq!(recorder.dropped_count(), 0);
    }

    #[test]
    fn attempt_rejects_observations_from_a_different_evidence_mode() {
        let recorder = InMemoryObservationRecorder::new(2).unwrap();
        let session = ProviderObservationSession::new(
            EvidenceMode::MetadataOnly,
            OpaqueScopeId::new("018f1234-5678-7abc-8def-0123456789ab").unwrap(),
            recorder.observer(),
        );
        let context = session.context("run-mode-check", Uuid::nil()).unwrap();
        let attempt = context.begin_attempt().unwrap();

        let mut mismatched = observation(attempt.attempt_number(), EvidenceMode::Disabled);
        mismatched.scope = attempt.scope().clone();
        assert_eq!(attempt.notify(mismatched), DeliveryDisposition::Dropped);
        assert!(recorder.is_empty());

        let mut synthetic = observation(attempt.attempt_number(), EvidenceMode::SyntheticPayloads);
        synthetic.scope = attempt.scope().clone();
        assert_eq!(attempt.notify(synthetic), DeliveryDisposition::Dropped);
        assert!(recorder.is_empty());

        let mut matching = observation(attempt.attempt_number(), EvidenceMode::MetadataOnly);
        matching.scope = attempt.scope().clone();
        assert_eq!(attempt.notify(matching), DeliveryDisposition::Delivered);
        assert_eq!(recorder.len(), 1);
    }

    #[test]
    fn disabled_session_rejects_an_enabled_observation_before_the_sink() {
        let recorder = InMemoryObservationRecorder::new(1).unwrap();
        let session = ProviderObservationSession::new(
            EvidenceMode::Disabled,
            OpaqueScopeId::new("018f1234-5678-7abc-8def-0123456789ab").unwrap(),
            recorder.observer(),
        );
        let context = session.context("run-disabled-mode", Uuid::nil()).unwrap();
        let attempt = context.begin_attempt().unwrap();
        let mut enabled = observation(attempt.attempt_number(), EvidenceMode::MetadataOnly);
        enabled.scope = attempt.scope().clone();

        assert_eq!(attempt.notify(enabled), DeliveryDisposition::Dropped);
        assert!(recorder.is_empty());
        assert_eq!(recorder.dropped_count(), 0);
    }

    struct PanicSink;

    impl ProviderObservationSink for PanicSink {
        fn observe(&self, _observation: ProviderObservation) -> ObservationAcceptance {
            panic!("observer callback panicked")
        }
    }

    #[test]
    fn observer_contains_callback_panics_without_returning_panic_text() {
        let observer = ProviderObserver::new(Arc::new(PanicSink));
        assert_eq!(
            observer.notify(observation(1, EvidenceMode::MetadataOnly)),
            DeliveryDisposition::Panicked
        );
    }

    #[test]
    fn serialization_contains_only_allowlisted_structural_fields() {
        let value = serde_json::to_value(observation(1, EvidenceMode::MetadataOnly)).unwrap();
        let serialized = value.to_string();
        for forbidden_content in ["Bearer ", "sk-", "https://", "/Users/", "alice@", "secret"] {
            assert!(!serialized.contains(forbidden_content));
        }
        assert_no_forbidden_keys(&value);
        assert!(serialized.contains("/v1/chat/completions"));
        assert!(!serialized.contains("authorization"));
    }

    fn assert_no_forbidden_keys(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for key in map.keys() {
                    let normalized = key.replace('_', "").to_ascii_lowercase();
                    for forbidden in [
                        "prompt",
                        "body",
                        "headervalue",
                        "endpointurl",
                        "errortext",
                        "hostpath",
                        "account",
                        "userid",
                        "teamid",
                        "principal",
                    ] {
                        assert_ne!(normalized, forbidden);
                    }
                }
                map.values().for_each(assert_no_forbidden_keys);
            }
            serde_json::Value::Array(items) => items.iter().for_each(assert_no_forbidden_keys),
            _ => {}
        }
    }

    #[test]
    fn debug_output_redacts_all_identity_values() {
        let output = format!("{:?}", observation(1, EvidenceMode::MetadataOnly));
        assert!(!output.contains(HASH_A));
        assert!(!output.contains("grok-code-fast-1"));
        assert!(!output.contains("018f1234"));
        assert!(output.contains("[redacted]"));
        assert!(output.contains("[opaque]"));
    }
}
