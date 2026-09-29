//! One-assignment provider capability for the managed Grok Build child.
//!
//! The child never receives an xAI credential. Its random, expiring bearer is
//! accepted only by this loopback relay. Every real upstream request uses the
//! existing credential store and canonical provider-send authority. Revocation
//! prevents future forwards; it cannot undo an already-sent provider action.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::grok_build::{CredentialLeaseHandle, CredentialLeaseResolver, GrokBuildAdapterError};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_REQUESTS: u32 = 6;
const MAX_OUTPUT_TOKENS: u32 = 1024;
const MAX_TOOL_CALLS: u32 = 12;
pub(crate) const MAX_TOTAL_TOKENS: u64 = 16_000;
const LEASE_LIFETIME_MS: u64 = 180_000;
// Diagnostic retention limits only. They do not change usage acceptance or
// the lease budget. Cost ticks need their own bound, not the token bound.
const MAX_OBSERVED_TOKEN_VALUE: u64 = 65_536;
const MAX_OBSERVED_COST_TICKS: u64 = 1_000_000_000_000;
const MAX_OBSERVED_DECIMAL_CHARS: usize = 32;
const MAX_USAGE_OBSERVATIONS: usize = 2;
const MAX_USAGE_SNAPSHOT_BYTES: usize = 3072;
const MAX_UNKNOWN_KEY_COUNT: usize = 4;

/// Host-minted confinement facts. No request can supply or change these.
#[derive(Clone, Debug)]
pub struct ManagedChildPolicy {
    pub(crate) endpoint: String,
    pub(crate) port: u16,
    pub(crate) model: String,
    pub(crate) max_duration_ms: u64,
    pub(crate) max_turns: u32,
    pub(crate) max_output_tokens: u32,
}

/// Stable categories contain no upstream prose, request bodies or credentials.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedProviderDiagnosticKind {
    BeforeWireRejected,
    TransportUncertain,
    HttpResponseObserved,
    HttpRejected,
    ProtocolFailure,
    UsageMissing,
    UsageInconsistent,
    ChildExit,
    ChildEvidenceMismatch,
    Cancelled,
    Expired,
    AbandonedForward,
    Completed,
    BudgetExceeded,
    SettlementFailure,
    RecoveryUncertain,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMachineError {
    InvalidRequestError,
    AuthenticationError,
    PermissionDenied,
    RateLimitExceeded,
    InsufficientQuota,
    InvalidApiKey,
    ModelNotFound,
    ContextLengthExceeded,
    ServerError,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedAdmissionDenial {
    Revoked,
    Uncertain,
    InFlight,
    Expired,
    RequestLimit,
    TokenReserve,
    DuplicateRequest,
    CredentialEcho,
}

/// Only allowlisted field names and bounded numbers may survive a rejected
/// provider receipt. These observations are never accepted accounting.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedUsageRejectionKind {
    MissingReceipt,
    RepeatedReceipt,
    MalformedReceipt,
    MissingField,
    MalformedField,
    UnsupportedField,
    MalformedDetails,
    UnsupportedDetailsField,
    TokenBoundExceeded,
    ConflictingTotal,
    UnsupportedSourceCount,
    ConflictingSubset,
    UnsupportedNonzeroDetail,
    CostNull,
    CostZero,
    CostNegative,
    CostOutOfRange,
    CostTypeUnsupported,
    AggregationPriorUnknown,
    AggregationMixedCostPresence,
    AggregationOverflow,
    AggregateBudgetExceeded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedUsageField {
    PromptTokens,
    CompletionTokens,
    TotalTokens,
    PromptTokensDetails,
    CompletionTokensDetails,
    TextTokens,
    AudioTokens,
    ImageTokens,
    CachedTokens,
    ReasoningTokens,
    AcceptedPredictionTokens,
    RejectedPredictionTokens,
    NumSourcesUsed,
    CostInUsdTicks,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedUsageValueState {
    Missing,
    Null,
    Boolean,
    String,
    Number,
    Array,
    Object,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUsageRejection {
    pub kind: ManagedUsageRejectionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<ManagedUsageField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_state: Option<ManagedUsageValueState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<u64>,
}

impl ManagedUsageRejection {
    fn new(kind: ManagedUsageRejectionKind, field: Option<ManagedUsageField>) -> Self {
        Self {
            kind,
            field,
            value_state: None,
            observed: None,
            expected: None,
        }
    }

    fn with_value_state(mut self, value: Option<&Value>) -> Self {
        self.value_state = Some(match value {
            None => ManagedUsageValueState::Missing,
            Some(Value::Null) => ManagedUsageValueState::Null,
            Some(Value::Bool(_)) => ManagedUsageValueState::Boolean,
            Some(Value::String(_)) => ManagedUsageValueState::String,
            Some(Value::Number(_)) => ManagedUsageValueState::Number,
            Some(Value::Array(_)) => ManagedUsageValueState::Array,
            Some(Value::Object(_)) => ManagedUsageValueState::Object,
        });
        self
    }

    fn with_numbers(mut self, observed: u64, expected: u64) -> Self {
        // An untrusted number must not become an unbounded telemetry channel.
        const MAX_RETAINED_NUMBER: u64 = 64 * 1024;
        self.observed = (observed <= MAX_RETAINED_NUMBER).then_some(observed);
        self.expected = (expected <= MAX_RETAINED_NUMBER).then_some(expected);
        self
    }
}

/// These are the only provider usage paths eligible for diagnostic retention.
/// The two audio paths deliberately have distinct identities.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ManagedObservedUsagePath {
    #[serde(rename = "prompt_tokens")]
    PromptTokens,
    #[serde(rename = "completion_tokens")]
    CompletionTokens,
    #[serde(rename = "total_tokens")]
    TotalTokens,
    #[serde(rename = "prompt_tokens_details.text_tokens")]
    PromptTextTokens,
    #[serde(rename = "prompt_tokens_details.audio_tokens")]
    PromptAudioTokens,
    #[serde(rename = "prompt_tokens_details.image_tokens")]
    PromptImageTokens,
    #[serde(rename = "prompt_tokens_details.cached_tokens")]
    PromptCachedTokens,
    #[serde(rename = "completion_tokens_details.reasoning_tokens")]
    CompletionReasoningTokens,
    #[serde(rename = "completion_tokens_details.audio_tokens")]
    CompletionAudioTokens,
    #[serde(rename = "completion_tokens_details.accepted_prediction_tokens")]
    CompletionAcceptedPredictionTokens,
    #[serde(rename = "completion_tokens_details.rejected_prediction_tokens")]
    CompletionRejectedPredictionTokens,
    #[serde(rename = "num_sources_used")]
    NumSourcesUsed,
    #[serde(rename = "cost_in_usd_ticks")]
    CostInUsdTicks,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedObservedValueState {
    Missing,
    Null,
    Zero,
    Positive,
    Negative,
    NonIntegral,
    WrongType,
    ParentUnavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedObservedUsageField {
    pub path: ManagedObservedUsagePath,
    pub state: ManagedObservedValueState,
    /// Exact signed integer when within this path's independent retention bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
    /// Canonical JSON number for a bounded non-integral value only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_integral_value: Option<serde_json::Number>,
    /// The state/sign remains available when an integer exceeds that bound.
    #[serde(default)]
    pub value_omitted: bool,
    /// Potentially credential-bearing string/array/object content is never copied.
    #[serde(default)]
    pub content_suppressed: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUnknownUsageKeys {
    pub count_up_to_four: u8,
    pub more: bool,
    pub content_suppressed: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedObservedContainerState {
    Missing,
    Null,
    Object,
    WrongType,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedObservedContainer {
    pub state: ManagedObservedContainerState,
    /// No malformed parent content is retained, even when it is a string.
    pub content_suppressed: bool,
}

fn observed_container(value: Option<&Value>) -> ManagedObservedContainer {
    let state = match value {
        None => ManagedObservedContainerState::Missing,
        Some(Value::Null) => ManagedObservedContainerState::Null,
        Some(Value::Object(_)) => ManagedObservedContainerState::Object,
        Some(_) => ManagedObservedContainerState::WrongType,
    };
    ManagedObservedContainer {
        state,
        content_suppressed: state == ManagedObservedContainerState::WrongType,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUsageSnapshot {
    pub receipt_ordinal: u8,
    pub receipt_state: ManagedObservedContainer,
    pub prompt_details_state: ManagedObservedContainer,
    pub completion_details_state: ManagedObservedContainer,
    /// Exactly 13 allowlisted paths; the array bounds serialized field count.
    pub fields: [ManagedObservedUsageField; 13],
    pub unknown_top_level_keys: Option<ManagedUnknownUsageKeys>,
    pub unknown_prompt_details_keys: Option<ManagedUnknownUsageKeys>,
    pub unknown_completion_details_keys: Option<ManagedUnknownUsageKeys>,
}

/// `None` on the containing evidence means a historical receipt with no
/// observation facility. An empty new observation is not a zero-token receipt.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUsageObservationEvidence {
    pub snapshots: Vec<ManagedUsageSnapshot>,
    pub observations_omitted: bool,
    pub credential_suppressed: bool,
}

impl ManagedUsageObservationEvidence {
    fn retain(&mut self, snapshots: impl IntoIterator<Item = ManagedUsageSnapshot>) {
        for snapshot in snapshots {
            let within_byte_bound = serde_json::to_vec(&snapshot)
                .is_ok_and(|encoded| encoded.len() <= MAX_USAGE_SNAPSHOT_BYTES);
            if self.snapshots.len() < MAX_USAGE_OBSERVATIONS && within_byte_bound {
                self.snapshots.push(snapshot);
            } else {
                self.observations_omitted = true;
            }
        }
    }
}

fn observed_unknown_keys(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> ManagedUnknownUsageKeys {
    // At most the known keys plus five unknown keys are inspected. The
    // response byte ceiling independently bounds the parsed JSON object.
    let count = object
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .take(MAX_UNKNOWN_KEY_COUNT + 1)
        .count();
    ManagedUnknownUsageKeys {
        count_up_to_four: count.min(MAX_UNKNOWN_KEY_COUNT) as u8,
        more: count > MAX_UNKNOWN_KEY_COUNT,
        content_suppressed: count != 0,
    }
}

fn observed_usage_field(
    path: ManagedObservedUsagePath,
    value: Option<&Value>,
    parent_available: bool,
) -> ManagedObservedUsageField {
    use ManagedObservedUsagePath as Path;
    use ManagedObservedValueState as State;
    let mut field = ManagedObservedUsageField {
        path,
        state: State::Missing,
        value: None,
        non_integral_value: None,
        value_omitted: false,
        content_suppressed: false,
    };
    if !parent_available {
        field.state = State::ParentUnavailable;
        return field;
    }
    let bound = if path == Path::CostInUsdTicks {
        MAX_OBSERVED_COST_TICKS
    } else {
        MAX_OBSERVED_TOKEN_VALUE
    };
    match value {
        None => {}
        Some(Value::Null) => field.state = State::Null,
        Some(Value::Number(number)) => {
            if let Some(integer) = number.as_u64() {
                field.state = if integer == 0 {
                    State::Zero
                } else {
                    State::Positive
                };
                if integer <= bound {
                    field.value = Some(integer as i64);
                } else {
                    field.value_omitted = true;
                }
            } else if let Some(integer) = number.as_i64() {
                field.state = State::Negative;
                if integer.unsigned_abs() <= bound {
                    field.value = Some(integer);
                } else {
                    field.value_omitted = true;
                }
            } else {
                field.state = State::NonIntegral;
                if number
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value.abs() <= bound as f64)
                    && number.to_string().len() <= MAX_OBSERVED_DECIMAL_CHARS
                {
                    field.non_integral_value = Some(number.clone());
                } else {
                    field.value_omitted = true;
                }
            }
        }
        Some(Value::Bool(_)) => field.state = State::WrongType,
        Some(Value::String(_) | Value::Array(_) | Value::Object(_)) => {
            field.state = State::WrongType;
            field.content_suppressed = true;
        }
    }
    field
}

fn observe_managed_usage(value: &Value, receipt_ordinal: u8) -> ManagedUsageSnapshot {
    use ManagedObservedUsagePath as Path;
    const TOP_LEVEL: &[&str] = &[
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "prompt_tokens_details",
        "completion_tokens_details",
        "num_sources_used",
        "cost_in_usd_ticks",
    ];
    const PROMPT_DETAILS: &[&str] = &[
        "text_tokens",
        "audio_tokens",
        "image_tokens",
        "cached_tokens",
    ];
    const COMPLETION_DETAILS: &[&str] = &[
        "reasoning_tokens",
        "audio_tokens",
        "accepted_prediction_tokens",
        "rejected_prediction_tokens",
    ];
    const FIELDS: [(Path, Option<&str>, &str); 13] = [
        (Path::PromptTokens, None, "prompt_tokens"),
        (Path::CompletionTokens, None, "completion_tokens"),
        (Path::TotalTokens, None, "total_tokens"),
        (Path::PromptTextTokens, Some("prompt"), "text_tokens"),
        (Path::PromptAudioTokens, Some("prompt"), "audio_tokens"),
        (Path::PromptImageTokens, Some("prompt"), "image_tokens"),
        (Path::PromptCachedTokens, Some("prompt"), "cached_tokens"),
        (
            Path::CompletionReasoningTokens,
            Some("completion"),
            "reasoning_tokens",
        ),
        (
            Path::CompletionAudioTokens,
            Some("completion"),
            "audio_tokens",
        ),
        (
            Path::CompletionAcceptedPredictionTokens,
            Some("completion"),
            "accepted_prediction_tokens",
        ),
        (
            Path::CompletionRejectedPredictionTokens,
            Some("completion"),
            "rejected_prediction_tokens",
        ),
        (Path::NumSourcesUsed, None, "num_sources_used"),
        (Path::CostInUsdTicks, None, "cost_in_usd_ticks"),
    ];
    let top = value.as_object();
    let prompt_parent = top.and_then(|object| object.get("prompt_tokens_details"));
    let completion_parent = top.and_then(|object| object.get("completion_tokens_details"));
    let prompt = prompt_parent.and_then(Value::as_object);
    let completion = completion_parent.and_then(Value::as_object);
    let fields = std::array::from_fn(|index| {
        let (path, parent, key) = FIELDS[index];
        let object = match parent {
            None => top,
            Some("prompt") => prompt,
            Some("completion") => completion,
            Some(_) => unreachable!("fixed allowlisted parent"),
        };
        observed_usage_field(
            path,
            object.and_then(|object| object.get(key)),
            object.is_some(),
        )
    });
    ManagedUsageSnapshot {
        receipt_ordinal,
        receipt_state: observed_container(Some(value)),
        prompt_details_state: observed_container(prompt_parent),
        completion_details_state: observed_container(completion_parent),
        fields,
        unknown_top_level_keys: top.map(|object| observed_unknown_keys(object, TOP_LEVEL)),
        unknown_prompt_details_keys: prompt
            .map(|object| observed_unknown_keys(object, PROMPT_DETAILS)),
        unknown_completion_details_keys: completion
            .map(|object| observed_unknown_keys(object, COMPLETION_DETAILS)),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedProviderDiagnostic {
    pub kind: ManagedProviderDiagnosticKind,
    pub admission: u32,
    pub http_status: Option<u16>,
    pub provider_request_id: Option<String>,
    pub provider_error_type: Option<ProviderMachineError>,
    pub provider_error_code: Option<ProviderMachineError>,
    #[serde(default)]
    pub admission_denial: Option<ManagedAdmissionDenial>,
    #[serde(default)]
    pub request_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_rejection: Option<ManagedUsageRejection>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedProviderEvidence {
    pub mechanism: String,
    pub model: String,
    pub requests_reserved: u32,
    pub wire_attempts: u32,
    pub responses_completed: u32,
    pub denied_requests: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Full prompt plus completion tokens. `None` is historical/unknown, not zero.
    #[serde(default)]
    pub total_tokens: Option<u64>,
    /// Subsets of the full prompt and completion totals, never added twice.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    /// Exact provider-reported ticks when present; may be partial unless
    /// `cost_complete` is true. Missing cost is never interpreted as free.
    #[serde(default)]
    pub cost_in_usd_ticks: Option<i64>,
    #[serde(default)]
    pub cost_missing_calls: u32,
    #[serde(default)]
    pub cost_complete: bool,
    pub usage_observed: bool,
    pub accounting_complete: bool,
    pub uncertain: bool,
    pub revoked: bool,
    pub authority_attempts: Vec<String>,
    #[serde(default)]
    pub http_responses_observed: u32,
    // None means the older receipt (or lost relay) did not establish this
    // fact. Deserializing historical uncertainty must not invent false.
    #[serde(default)]
    pub remote_effect_uncertain: Option<bool>,
    #[serde(default)]
    pub interruption: Option<ManagedProviderDiagnosticKind>,
    #[serde(default)]
    pub diagnostics: Vec<ManagedProviderDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_observation: Option<ManagedUsageObservationEvidence>,
    #[serde(default)]
    pub diagnostics_truncated: bool,
}

struct Lease {
    secret: String,
    issued: Option<Instant>,
    max_requests: u32,
    max_duration_ms: u64,
    max_total_tokens: u64,
    cancelled: CancellationToken,
    requests: BTreeSet<String>,
    evidence: ManagedProviderEvidence,
    in_flight: bool,
}

struct RelayState {
    credentials: crate::auth_store::WireCredentials,
    target: crate::host_helpers::ResolvedModelTarget,
    client: reqwest::Client,
    leases: Mutex<BTreeMap<String, Lease>>,
    stopped: CancellationToken,
    #[cfg(test)]
    completed_forward_handoff: Mutex<Option<CompletedForwardHandoff>>,
}

#[cfg(test)]
struct CompletedForwardHandoff {
    published: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

pub struct ManagedProviderRelay {
    state: Arc<RelayState>,
    policy: ManagedChildPolicy,
    credential_dir: PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for ManagedProviderRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedProviderRelay")
            .field("mechanism", &"host_relay_v1")
            .field("live", &!self.server.is_finished())
            .finish_non_exhaustive()
    }
}

impl ManagedProviderRelay {
    pub async fn start(model: &str, credential_dir: PathBuf) -> Result<Arc<Self>, &'static str> {
        if model != "grok-build-0.1" {
            return Err("managed relay requires the pinned grok-build-0.1 model");
        }
        let credentials = crate::auth_store::resolve_wire_credentials_for_model(model)
            .map_err(|_| "host credential acquisition failed")?
            .ok_or("host xAI credential is unavailable")?;
        let credentials = crate::auth_store::ensure_fresh_credentials(credentials).await;
        if credentials.bearer.is_empty()
            || credentials
                .expires_at
                .is_some_and(|expiry| expiry <= chrono::Utc::now())
        {
            return Err("host xAI credential is expired or unavailable");
        }
        let target = crate::host_helpers::resolve_model_target(&credentials, model)
            .map_err(|_| "managed provider model is unavailable")?;
        if credentials.provider_id != "xai"
            || !matches!(
                target.base_url.as_str(),
                "https://api.x.ai/v1" | "https://cli-chat-proxy.grok.com/v1"
            )
            || target.wire_model != "grok-build-0.1"
            || !target.capabilities.tools
            || !target.capabilities.stream
        {
            return Err("managed provider route or coding capability is unsupported");
        }
        Self::with_target(credentials, target, credential_dir).await
    }

    async fn with_target(
        credentials: crate::auth_store::WireCredentials,
        target: crate::host_helpers::ResolvedModelTarget,
        credential_dir: PathBuf,
    ) -> Result<Arc<Self>, &'static str> {
        fs::create_dir_all(&credential_dir).map_err(|_| "private lease directory unavailable")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::symlink_metadata(&credential_dir)
                .map_err(|_| "private lease directory unavailable")?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("private lease directory is not an owned directory");
            }
            fs::set_permissions(&credential_dir, fs::Permissions::from_mode(0o700))
                .map_err(|_| "private lease directory is not protected")?;
        }
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "managed relay cannot bind loopback")?;
        let port = listener
            .local_addr()
            .map_err(|_| "managed relay address unavailable")?
            .port();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|_| "managed provider transport unavailable")?;
        let policy = ManagedChildPolicy {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            model: target.wire_model.clone(),
            max_duration_ms: LEASE_LIFETIME_MS,
            max_turns: MAX_REQUESTS,
            max_output_tokens: MAX_OUTPUT_TOKENS,
        };
        let state = Arc::new(RelayState {
            credentials,
            target,
            client,
            leases: Mutex::new(BTreeMap::new()),
            stopped: CancellationToken::new(),
            #[cfg(test)]
            completed_forward_handoff: Mutex::new(None),
        });
        let router = Router::new()
            .route("/v1/models", get(models))
            .route("/v1/chat/completions", post(completion))
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .with_state(state.clone());
        let stop = state.stopped.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await;
        });
        Ok(Arc::new(Self {
            state,
            policy,
            credential_dir,
            server,
        }))
    }

    pub fn policy(&self) -> ManagedChildPolicy {
        self.policy.clone()
    }

    pub fn evidence(&self, lease_id: &str) -> Option<ManagedProviderEvidence> {
        self.state.leases.lock().ok()?.get(lease_id).map(|lease| {
            let mut evidence = lease.evidence.clone();
            evidence.accounting_complete = !lease.in_flight
                && !evidence.uncertain
                && evidence.responses_completed == evidence.wire_attempts;
            evidence.cost_complete = evidence.accounting_complete
                && evidence.cost_missing_calls == 0
                && evidence.cost_in_usd_ticks.is_some();
            evidence
        })
    }
}

impl Drop for ManagedProviderRelay {
    fn drop(&mut self) {
        self.state.stopped.cancel();
        if let Ok(mut leases) = self.state.leases.lock() {
            for (id, lease) in leases.iter_mut() {
                lease.cancelled.cancel();
                lease.evidence.revoked = true;
                let _ = fs::remove_file(self.credential_dir.join(id));
            }
        }
        self.server.abort();
    }
}

impl CredentialLeaseResolver for ManagedProviderRelay {
    fn provider_quiescent(&self, lease_id: &str) -> bool {
        self.state
            .leases
            .lock()
            .ok()
            .and_then(|leases| leases.get(lease_id).map(|lease| !lease.in_flight))
            .unwrap_or(false)
    }
    fn lease_id_for_request(
        &self,
        alias: &str,
        request: &str,
    ) -> Result<String, GrokBuildAdapterError> {
        if alias.is_empty()
            || request.is_empty()
            || self.server.is_finished()
            || self.state.stopped.is_cancelled()
        {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        let id = format!(
            "managed-{:x}",
            Sha256::digest(format!("{alias}\0{request}"))
        );
        let mut leases = self
            .state
            .leases
            .lock()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        if leases.contains_key(&id) {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        let secret = format!(
            "gptah-{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        leases.insert(
            id.clone(),
            Lease {
                secret,
                issued: None,
                max_requests: MAX_REQUESTS,
                max_duration_ms: LEASE_LIFETIME_MS,
                max_total_tokens: MAX_TOTAL_TOKENS,
                cancelled: CancellationToken::new(),
                requests: BTreeSet::new(),
                in_flight: false,
                evidence: ManagedProviderEvidence {
                    mechanism: "host_relay_v1".into(),
                    model: self.policy.model.clone(),
                    remote_effect_uncertain: Some(false),
                    usage_observation: Some(ManagedUsageObservationEvidence::default()),
                    ..Default::default()
                },
            },
        );
        Ok(id)
    }

    fn bind_lease_bounds(
        &self,
        id: &str,
        max_requests: u32,
        max_duration_ms: u64,
        max_total_tokens: u64,
    ) -> Result<(), GrokBuildAdapterError> {
        let mut leases = self
            .state
            .leases
            .lock()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        let lease = leases
            .get_mut(id)
            .ok_or(GrokBuildAdapterError::CredentialLease)?;
        if lease.issued.is_some()
            || lease.cancelled.is_cancelled()
            || max_requests == 0
            || max_requests > lease.max_requests
            || max_duration_ms == 0
            || max_duration_ms > lease.max_duration_ms
            || max_total_tokens == 0
            || max_total_tokens > lease.max_total_tokens
        {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        lease.max_requests = max_requests;
        lease.max_duration_ms = max_duration_ms;
        lease.max_total_tokens = max_total_tokens;
        Ok(())
    }

    fn resolve(&self, lease_id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        let mut leases = self
            .state
            .leases
            .lock()
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        let lease = leases
            .get_mut(lease_id)
            .ok_or(GrokBuildAdapterError::CredentialLease)?;
        if lease.issued.is_some() || lease.cancelled.is_cancelled() || self.server.is_finished() {
            return Err(GrokBuildAdapterError::CredentialLease);
        }
        lease.issued = Some(Instant::now());
        let state = Arc::downgrade(&self.state);
        let id = lease_id.to_owned();
        let cancel = lease.cancelled.clone();
        let duration_ms = lease.max_duration_ms;
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(duration_ms)) => {
                    if let Some(state) = state.upgrade() {
                        if let Ok(mut leases) = state.leases.lock() {
                            if let Some(lease) = leases.get_mut(&id) {
                                record_diagnostic(lease, ManagedProviderDiagnosticKind::Expired);
                                lease.evidence.uncertain |= lease.in_flight;
                                lease.evidence.remote_effect_uncertain = Some(lease.evidence.remote_effect_uncertain.unwrap_or(true) || lease.in_flight);
                                lease.evidence.revoked = true;
                                lease.cancelled.cancel();
                            }
                        }
                    }
                }
                _ = cancel.cancelled() => {}
            }
        });
        let path = self.credential_dir.join(lease_id);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        file.write_all(b"{}")
            .and_then(|_| file.sync_all())
            .map_err(|_| GrokBuildAdapterError::CredentialLease)?;
        let mut policy = self.policy.clone();
        policy.max_turns = lease.max_requests;
        policy.max_duration_ms = lease.max_duration_ms;
        Ok(CredentialLeaseHandle::from_managed_relay(
            path,
            policy,
            lease.secret.clone(),
        ))
    }

    fn revoke(&self, lease_id: &str) -> Result<(), GrokBuildAdapterError> {
        let mut leases = self
            .state
            .leases
            .lock()
            .map_err(|_| GrokBuildAdapterError::CredentialRevocation)?;
        let lease = leases
            .get_mut(lease_id)
            .ok_or(GrokBuildAdapterError::CredentialRevocation)?;
        if lease.in_flight {
            record_diagnostic(lease, ManagedProviderDiagnosticKind::Cancelled);
        }
        lease.cancelled.cancel();
        lease.evidence.uncertain |= lease.in_flight;
        lease.evidence.remote_effect_uncertain =
            Some(lease.evidence.remote_effect_uncertain.unwrap_or(true) || lease.in_flight);
        lease.evidence.revoked = true;
        let path = self.credential_dir.join(lease_id);
        if path.exists() {
            fs::remove_file(path).map_err(|_| GrokBuildAdapterError::CredentialRevocation)?;
        }
        Ok(())
    }

    fn revokes_upstream(&self) -> bool {
        true
    }
    fn managed_child_policy(&self) -> Option<ManagedChildPolicy> {
        Some(self.policy())
    }
    fn provider_evidence(&self, id: &str) -> Option<ManagedProviderEvidence> {
        self.evidence(id)
    }
    fn note_child_failure(&self, id: &str, kind: ManagedProviderDiagnosticKind) {
        note_diagnostic(&self.state, id, kind);
    }
    fn readiness_error(&self) -> Option<&'static str> {
        if self.server.is_finished() || self.state.stopped.is_cancelled() {
            Some("managed provider relay is not live")
        } else if self
            .state
            .credentials
            .expires_at
            .is_some_and(|expiry| expiry <= chrono::Utc::now())
        {
            Some("host provider credential has expired")
        } else {
            None
        }
    }
    fn stop_authority(&self) {
        self.state.stopped.cancel();
        if let Ok(mut leases) = self.state.leases.lock() {
            for lease in leases.values_mut() {
                if lease.in_flight {
                    record_diagnostic(lease, ManagedProviderDiagnosticKind::Cancelled);
                }
                lease.evidence.revoked = true;
                lease.cancelled.cancel();
            }
        }
        self.server.abort();
    }
}

fn authorized_lease(state: &RelayState, headers: &HeaderMap) -> Option<String> {
    if state.stopped.is_cancelled() {
        return None;
    }
    let bearer = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let leases = state.leases.lock().ok()?;
    leases.iter().find_map(|(id, lease)| {
        let active = lease
            .issued
            .is_some_and(|issued| issued.elapsed() < Duration::from_millis(lease.max_duration_ms))
            && !lease.cancelled.is_cancelled();
        let active = active && !lease.evidence.revoked && !lease.evidence.uncertain;
        (active && token_equal(bearer.as_bytes(), lease.secret.as_bytes())).then(|| id.clone())
    })
}

fn token_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |different, (a, b)| different | (a ^ b))
        == 0
}

async fn models(State(state): State<Arc<RelayState>>, headers: HeaderMap) -> Response {
    if authorized_lease(&state, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"object":"list","data":[{"id":state.target.wire_model,"object":"model","created":0,"owned_by":"xai"}]})).into_response()
}

async fn completion(
    State(state): State<Arc<RelayState>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let Some(id) = authorized_lease(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let result = forward(&state, &id, &bytes).await;
    match result {
        Ok(body) => ([("content-type", "text/event-stream")], body).into_response(),
        Err(reason) => {
            invalidate(&state, &id);
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":{"message":reason,"type":"managed_assignment_denied"}})),
            )
                .into_response()
        }
    }
}

async fn forward(state: &RelayState, id: &str, bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    let result = forward_inner(state, id, bytes).await;
    if result.is_err() {
        if let Ok(mut leases) = state.leases.lock() {
            if let Some(lease) = leases.get_mut(id) {
                if lease.evidence.interruption.is_none() {
                    record_diagnostic(lease, ManagedProviderDiagnosticKind::BeforeWireRejected);
                }
            }
        }
    }
    result
}

async fn forward_inner(
    state: &RelayState,
    id: &str,
    bytes: &[u8],
) -> Result<Vec<u8>, &'static str> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("managed request exceeds byte budget");
    }
    let mut body: Value =
        serde_json::from_slice(bytes).map_err(|_| "managed request is not JSON")?;
    let object = body
        .as_object_mut()
        .ok_or("managed request is not an object")?;
    const ALLOWED: &[&str] = &[
        "model",
        "messages",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
        "stream",
        "stream_options",
        "max_tokens",
        "max_completion_tokens",
        "reasoning_effort",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str()))
        || object.get("model").and_then(Value::as_str) != Some(&state.target.wire_model)
        || object.get("stream").and_then(Value::as_bool) != Some(true)
        || !object.get("messages").is_some_and(Value::is_array)
    {
        return Err("managed request route or protocol is unsupported");
    }
    let tools = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("managed tool definitions are missing")?;
    if tools.len() > 8
        || tools.iter().any(|tool| {
            tool["type"] != "function"
                || !matches!(
                    tool["function"]["name"].as_str(),
                    Some(
                        "read_file"
                            | "write"
                            | "search_replace"
                            | "list_dir"
                            | "grep"
                            | "search_tool"
                            | "use_tool"
                    )
                )
        })
    {
        return Err("managed tool definitions exceed the allowed protocol");
    }
    object.remove("max_completion_tokens");
    object.remove("reasoning_effort");
    object.insert("max_tokens".into(), json!(MAX_OUTPUT_TOKENS));
    object.insert("parallel_tool_calls".into(), json!(false));
    object.insert("stream_options".into(), json!({"include_usage":true}));
    let (cancel, secret) = {
        let mut leases = state
            .leases
            .lock()
            .map_err(|_| "managed lease state unavailable")?;
        let lease = leases.get_mut(id).ok_or("managed lease unavailable")?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let denial = if lease.cancelled.is_cancelled() || lease.evidence.revoked {
            Some(ManagedAdmissionDenial::Revoked)
        } else if lease.evidence.uncertain {
            Some(ManagedAdmissionDenial::Uncertain)
        } else if lease.in_flight {
            Some(ManagedAdmissionDenial::InFlight)
        } else if lease
            .issued
            .is_none_or(|issued| issued.elapsed() >= Duration::from_millis(lease.max_duration_ms))
        {
            Some(ManagedAdmissionDenial::Expired)
        } else if lease.evidence.requests_reserved >= lease.max_requests {
            Some(ManagedAdmissionDenial::RequestLimit)
        } else if lease
            .evidence
            .accounted_total_tokens()
            .and_then(|total| total.checked_add(bytes.len() as u64))
            .and_then(|total| total.checked_add(u64::from(MAX_OUTPUT_TOKENS)))
            .is_none_or(|reserved| reserved > lease.max_total_tokens)
        {
            // Reserve a conservative byte-sized prompt allowance and output
            // ceiling; no unknown turn is reusable as settled zero usage.
            Some(ManagedAdmissionDenial::TokenReserve)
        } else if lease.requests.contains(&digest) {
            Some(ManagedAdmissionDenial::DuplicateRequest)
        } else if bytes
            .windows(lease.secret.len())
            .any(|part| part == lease.secret.as_bytes())
            || bytes
                .windows(state.credentials.bearer.len())
                .any(|part| part == state.credentials.bearer.as_bytes())
        {
            Some(ManagedAdmissionDenial::CredentialEcho)
        } else {
            None
        };
        if let Some(denial) = denial {
            lease.evidence.denied_requests = lease.evidence.denied_requests.saturating_add(1);
            record_diagnostic(lease, ManagedProviderDiagnosticKind::BeforeWireRejected);
            if let Some(last) = lease.evidence.diagnostics.last_mut() {
                last.admission_denial = Some(denial);
                last.request_bytes = Some(bytes.len() as u32);
            }
            return Err("managed lease expired, repeated, or exceeded its budget");
        }
        lease.requests.insert(digest);
        lease.evidence.requests_reserved += 1;
        lease.in_flight = true;
        (lease.cancelled.clone(), lease.secret.clone())
    };
    let mut settlement = ForwardSettlement {
        state,
        id,
        completed: false,
    };
    let base = &state.target.base_url;
    let request = crate::auth_store::apply_auth_headers(
        state
            .client
            .post(format!("{}/chat/completions", base.trim_end_matches('/')))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("x-grok-effort", "low"),
        &state.credentials,
        base,
    );
    let request =
        bind_official_proxy_model_override(request, &state.target, &state.credentials, &body)?
            .json(&body);
    let target_scope = format!("managed-grok:{id}");
    let observe = |attempt: &str| -> anyhow::Result<()> {
        let mut leases = state
            .leases
            .lock()
            .map_err(|_| anyhow::anyhow!("managed send observation unavailable"))?;
        let lease = leases
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("managed lease unavailable"))?;
        lease.evidence.wire_attempts += 1;
        lease.evidence.remote_effect_uncertain = Some(true);
        lease.evidence.authority_attempts.push(attempt.to_owned());
        Ok(())
    };
    let response = crate::provider_transport::send_provider_request_observed(
        &state.client,
        request,
        crate::provider_transport::ProviderRequestScope {
            credential_secret: state.credentials.bearer.as_bytes(),
            dialect: "xai_chat_completions",
            model: &state.target.wire_model,
            target_scope: &target_scope,
        },
        Some(&cancel),
        &observe,
    )
    .await;
    let mut response = match response {
        Ok(response) => response,
        Err(error) => {
            if !error.is_uncertain() {
                if let Ok(mut leases) = state.leases.lock() {
                    if let Some(lease) = leases.get_mut(id) {
                        // Canonical transport proved this request did not
                        // reach the wire. Admission is still counted.
                        lease.evidence.remote_effect_uncertain = Some(false);
                    }
                }
            }
            note_diagnostic(
                state,
                id,
                if error.is_uncertain() {
                    ManagedProviderDiagnosticKind::TransportUncertain
                } else {
                    ManagedProviderDiagnosticKind::BeforeWireRejected
                },
            );
            invalidate(state, id);
            return Err("managed provider send failed; no automatic retry");
        }
    };
    let status = response.status().as_u16();
    let request_id = safe_request_id(response.headers(), &secret, &state.credentials.bearer);
    if let Ok(mut leases) = state.leases.lock() {
        if let Some(lease) = leases.get_mut(id) {
            lease.evidence.http_responses_observed += 1;
            record_diagnostic(lease, ManagedProviderDiagnosticKind::HttpResponseObserved);
            if let Some(last) = lease.evidence.diagnostics.last_mut() {
                last.http_status = Some(status);
                last.provider_request_id = request_id;
            }
        }
    }
    if !response.status().is_success() {
        // Parse only a small complete JSON error envelope. Free-form messages,
        // arbitrary headers and unknown codes are discarded, never retained.
        let mut error_body = Vec::new();
        let mut complete = false;
        loop {
            match response.next_chunk(Some(&cancel)).await {
                Ok(Some(chunk)) if error_body.len() + chunk.len() <= 4096 => {
                    error_body.extend_from_slice(&chunk)
                }
                Ok(None) => {
                    complete = true;
                    break;
                }
                _ => break,
            }
        }
        let codes = if complete {
            safe_error_codes(&error_body, &secret, &state.credentials.bearer)
        } else {
            (None, None)
        };
        if let Ok(mut leases) = state.leases.lock() {
            if let Some(lease) = leases.get_mut(id) {
                record_diagnostic(lease, ManagedProviderDiagnosticKind::HttpRejected);
                if let Some(last) = lease.evidence.diagnostics.last_mut() {
                    last.http_status = Some(status);
                    last.provider_error_type = codes.0;
                    last.provider_error_code = codes.1;
                }
            }
        }
        if complete {
            let _ = response.settle_http_failure("managed provider HTTP rejection");
        } else {
            let _ = response.settle_protocol_error("managed provider error envelope incomplete");
        }
        invalidate(state, id);
        return Err("managed provider rejected the request; no automatic retry");
    }
    let mut output = Vec::new();
    loop {
        match response.next_chunk(Some(&cancel)).await {
            Ok(Some(chunk)) if output.len() + chunk.len() <= MAX_RESPONSE_BYTES => {
                output.extend_from_slice(&chunk)
            }
            Ok(Some(_)) => {
                note_diagnostic(state, id, ManagedProviderDiagnosticKind::ProtocolFailure);
                invalidate(state, id);
                return Err("managed provider response exceeded bounds or became uncertain");
            }
            Err(_) => {
                note_diagnostic(state, id, ManagedProviderDiagnosticKind::TransportUncertain);
                invalidate(state, id);
                return Err("managed provider response exceeded bounds or became uncertain");
            }
            Ok(None) => break,
        }
    }
    let summary = match validate_completion(&output, &secret, &state.credentials.bearer) {
        Ok(summary) => summary,
        Err(failure) => {
            let message = failure.message;
            note_completion_failure(state, id, failure);
            let _ = response.settle_protocol_error(message);
            invalidate(state, id);
            return Err(message);
        }
    };
    {
        let mut leases = state
            .leases
            .lock()
            .map_err(|_| "managed lease state unavailable")?;
        let lease = leases.get_mut(id).ok_or("managed lease unavailable")?;
        if lease.cancelled.is_cancelled()
            || lease.issued.is_none_or(|issued| {
                issued.elapsed() >= Duration::from_millis(lease.max_duration_ms)
            })
            || lease.evidence.tool_calls + summary.0 > MAX_TOOL_CALLS
        {
            drop(leases);
            let _ =
                response.settle_protocol_error("managed lease cancelled or exceeded tool budget");
            invalidate(state, id);
            return Err("managed lease cancelled or exceeded tool budget");
        }
        lease
            .evidence
            .retain_usage_observations([summary.2.clone()]);
        lease.evidence.tool_calls += summary.0;
        let next = match lease.evidence.checked_accounting_after(summary.1) {
            Ok(next) => next,
            Err(rejection) => {
                record_diagnostic(lease, ManagedProviderDiagnosticKind::UsageInconsistent);
                if let Some(last) = lease.evidence.diagnostics.last_mut() {
                    last.usage_rejection = Some(rejection);
                }
                drop(leases);
                let _ = response.settle_protocol_error("managed usage aggregation is unsupported");
                invalidate(state, id);
                return Err("managed usage aggregation is unsupported");
            }
        };
        if next
            .total_tokens
            .is_none_or(|total| total > lease.max_total_tokens)
        {
            record_diagnostic(lease, ManagedProviderDiagnosticKind::BudgetExceeded);
            if let Some(last) = lease.evidence.diagnostics.last_mut() {
                let mut rejection = ManagedUsageRejection::new(
                    ManagedUsageRejectionKind::AggregateBudgetExceeded,
                    Some(ManagedUsageField::TotalTokens),
                );
                if let Some(total) = next.total_tokens {
                    rejection = rejection.with_numbers(total, lease.max_total_tokens);
                }
                last.usage_rejection = Some(rejection);
            }
            drop(leases);
            let _ = response.settle_protocol_error("managed aggregate token budget exceeded");
            invalidate(state, id);
            return Err("managed aggregate token budget exceeded");
        }
        lease.evidence = next;
    }
    if response.settle_success().is_err() {
        note_diagnostic(state, id, ManagedProviderDiagnosticKind::SettlementFailure);
        invalidate(state, id);
        return Err("managed provider settlement is not durable");
    }
    if let Ok(mut leases) = state.leases.lock() {
        if let Some(lease) = leases.get_mut(id) {
            if lease.cancelled.is_cancelled() {
                return Err("managed lease was revoked before response delivery");
            }
            lease.evidence.responses_completed += 1;
            lease.evidence.remote_effect_uncertain = Some(false);
            record_diagnostic(lease, ManagedProviderDiagnosticKind::Completed);
            lease.in_flight = false;
            settlement.completed = true;
        }
    }
    // Only tests pause at this actual post-publication/pre-Drop boundary.
    // Neither the lease mutex nor the hook mutex is held across the barrier.
    #[cfg(test)]
    let handoff = state.completed_forward_handoff.lock().unwrap().take();
    #[cfg(test)]
    if let Some(handoff) = handoff {
        let _ = handoff.published.send(());
        let _ = handoff.release.await;
    }
    Ok(output)
}

/// The child cannot supply upstream headers. On the OIDC CLI-proxy route,
/// bind the native CLI model override to the host-sealed target and validated
/// body model; leave public API and compatible-provider routes unchanged.
fn bind_official_proxy_model_override(
    request: reqwest::RequestBuilder,
    target: &crate::host_helpers::ResolvedModelTarget,
    credentials: &crate::auth_store::WireCredentials,
    body: &Value,
) -> Result<reqwest::RequestBuilder, &'static str> {
    if body.get("model").and_then(Value::as_str) != Some(target.wire_model.as_str()) {
        return Err("managed body model does not match host target");
    }
    if credentials.oidc_token_auth && target.base_url == "https://cli-chat-proxy.grok.com/v1" {
        Ok(request.header("x-grok-model-override", &target.wire_model))
    } else {
        Ok(request)
    }
}

/// A revoked child cannot issue another request while the host settles a
/// cancelled physical send. This guard also handles cancelled relay futures.
struct ForwardSettlement<'a> {
    state: &'a RelayState,
    id: &'a str,
    completed: bool,
}
impl Drop for ForwardSettlement<'_> {
    fn drop(&mut self) {
        // Successful publication already released this forward's ownership.
        // A successor may now be active; completed cleanup must not touch it.
        if self.completed {
            return;
        }
        if let Ok(mut leases) = self.state.leases.lock() {
            if let Some(lease) = leases.get_mut(self.id) {
                if !self.completed {
                    if lease.evidence.interruption.is_none() {
                        record_diagnostic(lease, ManagedProviderDiagnosticKind::AbandonedForward);
                    }
                    // The send observer marks remote uncertainty; a proven
                    // before-wire failure may clear it without refunding the
                    // admission or restoring this capability.
                    // Clearing local in-flight state cannot restore authority
                    // or reclaim unknown usage after an abandoned forward.
                    lease.evidence.uncertain |=
                        lease.evidence.responses_completed < lease.evidence.wire_attempts;
                    lease.evidence.revoked = true;
                    lease.cancelled.cancel();
                }
                lease.in_flight = false;
            }
        }
    }
}

fn record_diagnostic(lease: &mut Lease, kind: ManagedProviderDiagnosticKind) {
    lease.evidence.record_diagnostic(kind);
}

impl ManagedProviderEvidence {
    fn accounted_total_tokens(&self) -> Option<u64> {
        match (self.usage_observed, self.total_tokens) {
            (false, None)
                if self.input_tokens == 0
                    && self.output_tokens == 0
                    && self.cache_read_input_tokens == 0
                    && self.reasoning_tokens == 0
                    && self.cost_in_usd_ticks.is_none()
                    && self.cost_missing_calls == 0 =>
            {
                Some(0) // New lease, no provider usage yet.
            }
            (true, Some(total)) => Some(total),
            _ => None, // A legacy/inconsistent total cannot grant authority.
        }
    }

    fn checked_accounting_after(&self, usage: ManagedUsage) -> Result<Self, ManagedUsageRejection> {
        // A mixture of priced and unpriced calls cannot establish the full
        // charge. Keep the earlier evidence and fail this send closed.
        if self.usage_observed
            && (self.cost_in_usd_ticks.is_some() != usage.cost_in_usd_ticks.is_some())
        {
            return Err(ManagedUsageRejection::new(
                ManagedUsageRejectionKind::AggregationMixedCostPresence,
                Some(ManagedUsageField::CostInUsdTicks),
            ));
        }
        let mut next = self.clone();
        let overflow = |field| {
            ManagedUsageRejection::new(ManagedUsageRejectionKind::AggregationOverflow, Some(field))
        };
        next.input_tokens = next
            .input_tokens
            .checked_add(usage.input_tokens)
            .ok_or_else(|| overflow(ManagedUsageField::PromptTokens))?;
        next.output_tokens = next
            .output_tokens
            .checked_add(usage.output_tokens)
            .ok_or_else(|| overflow(ManagedUsageField::CompletionTokens))?;
        next.reasoning_tokens = next
            .reasoning_tokens
            .checked_add(usage.reasoning_tokens)
            .ok_or_else(|| overflow(ManagedUsageField::ReasoningTokens))?;
        next.cache_read_input_tokens = next
            .cache_read_input_tokens
            .checked_add(usage.cache_read_input_tokens)
            .ok_or_else(|| overflow(ManagedUsageField::CachedTokens))?;
        next.total_tokens = Some(
            self.accounted_total_tokens()
                .ok_or_else(|| {
                    ManagedUsageRejection::new(
                        ManagedUsageRejectionKind::AggregationPriorUnknown,
                        Some(ManagedUsageField::TotalTokens),
                    )
                })?
                .checked_add(usage.total_tokens)
                .ok_or_else(|| overflow(ManagedUsageField::TotalTokens))?,
        );
        if let Some(cost) = usage.cost_in_usd_ticks {
            next.cost_in_usd_ticks = Some(
                next.cost_in_usd_ticks
                    .unwrap_or(0)
                    .checked_add(cost)
                    .ok_or_else(|| overflow(ManagedUsageField::CostInUsdTicks))?,
            );
        } else {
            next.cost_missing_calls = next.cost_missing_calls.checked_add(1).ok_or_else(|| {
                ManagedUsageRejection::new(
                    ManagedUsageRejectionKind::AggregationOverflow,
                    Some(ManagedUsageField::CostInUsdTicks),
                )
            })?;
        }
        next.usage_observed = true;
        Ok(next)
    }

    pub(crate) fn record_diagnostic(&mut self, kind: ManagedProviderDiagnosticKind) {
        if !matches!(
            kind,
            ManagedProviderDiagnosticKind::HttpResponseObserved
                | ManagedProviderDiagnosticKind::Completed
        ) && self.interruption.is_none()
        {
            self.interruption = Some(kind);
        }
        let diagnostic = ManagedProviderDiagnostic {
            kind,
            admission: self.wire_attempts,
            http_status: None,
            provider_request_id: None,
            provider_error_type: None,
            provider_error_code: None,
            admission_denial: None,
            request_bytes: None,
            usage_rejection: None,
        };
        if self.diagnostics.len() < 32 {
            self.diagnostics.push(diagnostic);
        } else if let Some(last) = self.diagnostics.last_mut() {
            self.diagnostics_truncated = true;
            *last = diagnostic;
        }
    }

    fn retain_usage_observations(
        &mut self,
        snapshots: impl IntoIterator<Item = ManagedUsageSnapshot>,
    ) {
        self.usage_observation
            .get_or_insert_with(ManagedUsageObservationEvidence::default)
            .retain(snapshots);
    }
}
fn note_diagnostic(state: &RelayState, id: &str, kind: ManagedProviderDiagnosticKind) {
    if let Ok(mut leases) = state.leases.lock() {
        if let Some(lease) = leases.get_mut(id) {
            record_diagnostic(lease, kind);
        }
    }
}

fn note_completion_failure(state: &RelayState, id: &str, failure: CompletionValidationFailure) {
    if let Ok(mut leases) = state.leases.lock() {
        if let Some(lease) = leases.get_mut(id) {
            lease
                .evidence
                .retain_usage_observations(failure.usage_observations);
            if failure.credential_suppressed {
                lease
                    .evidence
                    .usage_observation
                    .get_or_insert_with(ManagedUsageObservationEvidence::default)
                    .credential_suppressed = true;
            }
            record_diagnostic(lease, failure.kind);
            if let Some(rejection) = failure.usage_rejection {
                if let Some(last) = lease.evidence.diagnostics.last_mut() {
                    last.usage_rejection = Some(rejection);
                }
            }
        }
    }
}

fn safe_request_id(headers: &HeaderMap, child: &str, upstream: &str) -> Option<String> {
    let value = headers
        .get("x-request-id")
        .or_else(|| headers.get("request-id"))?
        .to_str()
        .ok()?;
    if value.len() > 64 || value.contains(child) || value.contains(upstream) {
        return None;
    }
    if uuid::Uuid::parse_str(value).is_ok()
        || value
            .strip_prefix("req_")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        Some(value.to_owned())
    } else {
        None
    }
}

fn safe_error_codes(
    body: &[u8],
    child: &str,
    upstream: &str,
) -> (Option<ProviderMachineError>, Option<ProviderMachineError>) {
    fn code(value: &Value, child: &str, upstream: &str) -> Option<ProviderMachineError> {
        let text = value.as_str()?;
        if text.contains(child) || text.contains(upstream) {
            return None;
        }
        Some(match text {
            "invalid_request_error" => ProviderMachineError::InvalidRequestError,
            "authentication_error" => ProviderMachineError::AuthenticationError,
            "permission_denied" => ProviderMachineError::PermissionDenied,
            "rate_limit_exceeded" | "rate_limit_error" => ProviderMachineError::RateLimitExceeded,
            "insufficient_quota" => ProviderMachineError::InsufficientQuota,
            "invalid_api_key" => ProviderMachineError::InvalidApiKey,
            "model_not_found" => ProviderMachineError::ModelNotFound,
            "context_length_exceeded" => ProviderMachineError::ContextLengthExceeded,
            "server_error" => ProviderMachineError::ServerError,
            _ => return None,
        })
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return (None, None);
    };
    (
        code(&value["error"]["type"], child, upstream),
        code(&value["error"]["code"], child, upstream),
    )
}

fn invalidate(state: &RelayState, id: &str) {
    if let Ok(mut leases) = state.leases.lock() {
        if let Some(lease) = leases.get_mut(id) {
            lease.evidence.uncertain |=
                lease.evidence.responses_completed < lease.evidence.wire_attempts;
            lease.evidence.revoked = true;
            lease.cancelled.cancel();
        }
    }
}

/// One normalized Chat Completions usage receipt. Prompt includes cache hits;
/// reasoning is a completion subset in the installed CLI's current contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagedUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cache_read_input_tokens: u64,
    reasoning_tokens: u64,
    cost_in_usd_ticks: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
struct ManagedUsageValidationFailure {
    message: &'static str,
    rejection: ManagedUsageRejection,
}

impl ManagedUsageValidationFailure {
    fn new(
        message: &'static str,
        kind: ManagedUsageRejectionKind,
        field: Option<ManagedUsageField>,
    ) -> Self {
        Self {
            message,
            rejection: ManagedUsageRejection::new(kind, field),
        }
    }

    fn with_value_state(mut self, value: Option<&Value>) -> Self {
        self.rejection = self.rejection.with_value_state(value);
        self
    }

    fn with_numbers(mut self, observed: u64, expected: u64) -> Self {
        self.rejection = self.rejection.with_numbers(observed, expected);
        self
    }
}

#[derive(Clone, Debug)]
struct CompletionValidationFailure {
    message: &'static str,
    kind: ManagedProviderDiagnosticKind,
    usage_rejection: Option<ManagedUsageRejection>,
    usage_observations: Vec<ManagedUsageSnapshot>,
    credential_suppressed: bool,
}

impl CompletionValidationFailure {
    fn usage(
        message: &'static str,
        kind: ManagedProviderDiagnosticKind,
        rejection: ManagedUsageRejectionKind,
    ) -> Self {
        Self {
            message,
            kind,
            usage_rejection: Some(ManagedUsageRejection::new(rejection, None)),
            usage_observations: Vec::new(),
            credential_suppressed: false,
        }
    }

    fn with_observations(mut self, observations: Vec<ManagedUsageSnapshot>) -> Self {
        self.usage_observations = observations;
        self
    }
}

impl From<&'static str> for CompletionValidationFailure {
    fn from(message: &'static str) -> Self {
        Self {
            message,
            kind: ManagedProviderDiagnosticKind::ProtocolFailure,
            usage_rejection: None,
            usage_observations: Vec::new(),
            credential_suppressed: false,
        }
    }
}

impl From<ManagedUsageValidationFailure> for CompletionValidationFailure {
    fn from(failure: ManagedUsageValidationFailure) -> Self {
        Self {
            message: failure.message,
            kind: ManagedProviderDiagnosticKind::UsageInconsistent,
            usage_rejection: Some(failure.rejection),
            usage_observations: Vec::new(),
            credential_suppressed: false,
        }
    }
}

fn parse_managed_usage(value: &Value) -> Result<ManagedUsage, ManagedUsageValidationFailure> {
    use ManagedUsageField as Field;
    use ManagedUsageRejectionKind as Kind;

    let object = value.as_object().ok_or_else(|| {
        ManagedUsageValidationFailure::new(
            "managed usage is malformed",
            Kind::MalformedReceipt,
            None,
        )
        .with_value_state(Some(value))
    })?;
    const ALLOWED: &[&str] = &[
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "prompt_tokens_details",
        "completion_tokens_details",
        "cost_in_usd_ticks",
        "num_sources_used",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(ManagedUsageValidationFailure::new(
            "managed usage has unsupported fields",
            Kind::UnsupportedField,
            None,
        ));
    }
    let number = |key: &str, field: Field| {
        let value = object.get(key).ok_or_else(|| {
            ManagedUsageValidationFailure::new(
                "managed usage is malformed",
                Kind::MissingField,
                Some(field),
            )
            .with_value_state(None)
        })?;
        value.as_u64().ok_or_else(|| {
            ManagedUsageValidationFailure::new(
                "managed usage is malformed",
                Kind::MalformedField,
                Some(field),
            )
            .with_value_state(Some(value))
        })
    };
    let input_tokens = number("prompt_tokens", Field::PromptTokens)?;
    let output_tokens = number("completion_tokens", Field::CompletionTokens)?;
    let total_tokens = number("total_tokens", Field::TotalTokens)?;
    const INCONSISTENT: &str = "managed usage is inconsistent or exceeds token bounds";
    if input_tokens > MAX_REQUEST_BYTES as u64 {
        return Err(ManagedUsageValidationFailure::new(
            INCONSISTENT,
            Kind::TokenBoundExceeded,
            Some(Field::PromptTokens),
        )
        .with_numbers(input_tokens, MAX_REQUEST_BYTES as u64));
    }
    if output_tokens > u64::from(MAX_OUTPUT_TOKENS) {
        return Err(ManagedUsageValidationFailure::new(
            INCONSISTENT,
            Kind::TokenBoundExceeded,
            Some(Field::CompletionTokens),
        )
        .with_numbers(output_tokens, u64::from(MAX_OUTPUT_TOKENS)));
    }
    let expected_total = input_tokens.checked_add(output_tokens);
    if expected_total != Some(total_tokens) {
        // Never infer that a rejected additive-reasoning form is accepted.
        return Err(ManagedUsageValidationFailure::new(
            INCONSISTENT,
            Kind::ConflictingTotal,
            Some(Field::TotalTokens),
        )
        .with_numbers(total_tokens, expected_total.unwrap_or(u64::MAX)));
    }
    if let Some(value) = object.get("num_sources_used") {
        if value.as_u64() != Some(0) {
            let mut failure = ManagedUsageValidationFailure::new(
                INCONSISTENT,
                Kind::UnsupportedSourceCount,
                Some(Field::NumSourcesUsed),
            )
            .with_value_state(Some(value));
            if let Some(observed) = value.as_u64() {
                failure = failure.with_numbers(observed, 0);
            }
            return Err(failure);
        }
    }
    let details =
        |key: &str,
         field: Field,
         allowed: &[&str]|
         -> Result<Option<&serde_json::Map<String, Value>>, ManagedUsageValidationFailure> {
            let Some(value) = object.get(key) else {
                return Ok(None);
            };
            let nested = value.as_object().ok_or_else(|| {
                ManagedUsageValidationFailure::new(
                    "managed usage details are malformed",
                    Kind::MalformedDetails,
                    Some(field),
                )
                .with_value_state(Some(value))
            })?;
            if nested
                .keys()
                .any(|field| !allowed.contains(&field.as_str()))
            {
                return Err(ManagedUsageValidationFailure::new(
                    "managed usage details have unsupported fields",
                    Kind::UnsupportedDetailsField,
                    Some(field),
                ));
            }
            Ok(Some(nested))
        };
    let prompt = details(
        "prompt_tokens_details",
        Field::PromptTokensDetails,
        &[
            "text_tokens",
            "audio_tokens",
            "image_tokens",
            "cached_tokens",
        ],
    )?;
    let completion = details(
        "completion_tokens_details",
        Field::CompletionTokensDetails,
        &[
            "reasoning_tokens",
            "audio_tokens",
            "accepted_prediction_tokens",
            "rejected_prediction_tokens",
        ],
    )?;
    let optional = |details: Option<&serde_json::Map<String, Value>>,
                    key: &str,
                    field: Field|
     -> Result<u64, ManagedUsageValidationFailure> {
        details.and_then(|d| d.get(key)).map_or(Ok(0), |v| {
            v.as_u64().ok_or_else(|| {
                ManagedUsageValidationFailure::new(
                    "managed usage details are malformed",
                    Kind::MalformedField,
                    Some(field),
                )
                .with_value_state(Some(v))
            })
        })
    };
    let cache_read_input_tokens = optional(prompt, "cached_tokens", Field::CachedTokens)?;
    let reasoning_tokens = optional(completion, "reasoning_tokens", Field::ReasoningTokens)?;
    const DETAILS_INCONSISTENT: &str = "managed usage details are inconsistent or unsupported";
    if cache_read_input_tokens > input_tokens {
        return Err(ManagedUsageValidationFailure::new(
            DETAILS_INCONSISTENT,
            Kind::ConflictingSubset,
            Some(Field::CachedTokens),
        )
        .with_numbers(cache_read_input_tokens, input_tokens));
    }
    if reasoning_tokens > output_tokens {
        return Err(ManagedUsageValidationFailure::new(
            DETAILS_INCONSISTENT,
            Kind::ConflictingSubset,
            Some(Field::ReasoningTokens),
        )
        .with_numbers(reasoning_tokens, output_tokens));
    }
    if let Some(value) = prompt.and_then(|p| p.get("text_tokens")) {
        let text_tokens = value.as_u64().ok_or_else(|| {
            ManagedUsageValidationFailure::new(
                DETAILS_INCONSISTENT,
                Kind::MalformedField,
                Some(Field::TextTokens),
            )
            .with_value_state(Some(value))
        })?;
        if text_tokens != input_tokens {
            return Err(ManagedUsageValidationFailure::new(
                DETAILS_INCONSISTENT,
                Kind::ConflictingSubset,
                Some(Field::TextTokens),
            )
            .with_numbers(text_tokens, input_tokens));
        }
    }
    for (details, key, field) in [
        (prompt, "audio_tokens", Field::AudioTokens),
        (prompt, "image_tokens", Field::ImageTokens),
        (completion, "audio_tokens", Field::AudioTokens),
        (
            completion,
            "accepted_prediction_tokens",
            Field::AcceptedPredictionTokens,
        ),
        (
            completion,
            "rejected_prediction_tokens",
            Field::RejectedPredictionTokens,
        ),
    ] {
        let observed = optional(details, key, field)?;
        if observed != 0 {
            return Err(ManagedUsageValidationFailure::new(
                DETAILS_INCONSISTENT,
                Kind::UnsupportedNonzeroDetail,
                Some(field),
            )
            .with_numbers(observed, 0));
        }
    }
    let cost_in_usd_ticks = match object.get("cost_in_usd_ticks") {
        None => None,
        Some(Value::Null) => {
            return Err(ManagedUsageValidationFailure::new(
                "managed usage cost is unsupported",
                Kind::CostNull,
                Some(Field::CostInUsdTicks),
            )
            .with_value_state(Some(&Value::Null)));
        }
        Some(value) => match value.as_i64() {
            Some(ticks) if ticks > 0 => Some(ticks),
            Some(0) => {
                return Err(ManagedUsageValidationFailure::new(
                    "managed usage cost is unsupported",
                    Kind::CostZero,
                    Some(Field::CostInUsdTicks),
                )
                .with_value_state(Some(value))
                .with_numbers(0, 1));
            }
            Some(_) => {
                return Err(ManagedUsageValidationFailure::new(
                    "managed usage cost is unsupported",
                    Kind::CostNegative,
                    Some(Field::CostInUsdTicks),
                )
                .with_value_state(Some(value)));
            }
            None if value.as_u64().is_some() => {
                return Err(ManagedUsageValidationFailure::new(
                    "managed usage cost is unsupported",
                    Kind::CostOutOfRange,
                    Some(Field::CostInUsdTicks),
                )
                .with_value_state(Some(value)));
            }
            None => {
                return Err(ManagedUsageValidationFailure::new(
                    "managed usage cost is unsupported",
                    Kind::CostTypeUnsupported,
                    Some(Field::CostInUsdTicks),
                )
                .with_value_state(Some(value)));
            }
        },
    };
    Ok(ManagedUsage {
        input_tokens,
        output_tokens,
        total_tokens,
        cache_read_input_tokens,
        reasoning_tokens,
        cost_in_usd_ticks,
    })
}

type CompletionSummary = (u32, ManagedUsage, ManagedUsageSnapshot);
fn validate_completion(
    bytes: &[u8],
    child_secret: &str,
    upstream_secret: &str,
) -> Result<CompletionSummary, CompletionValidationFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| "managed response is not UTF-8")?;
    if text.contains(child_secret) || text.contains(upstream_secret) {
        let mut failure: CompletionValidationFailure =
            "managed response contains credential material".into();
        failure.credential_suppressed = true;
        return Err(failure);
    }
    let mut finished = false;
    let mut done = false;
    let mut tools = BTreeMap::<u64, (String, String)>::new();
    let mut usage: Option<(ManagedUsage, ManagedUsageSnapshot)> = None;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if done {
            return Err("managed stream contains data after its end marker".into());
        }
        if data == "[DONE]" {
            done = true;
            continue;
        }
        let value: Value = serde_json::from_str(data).map_err(|_| "managed stream is malformed")?;
        if value.get("error").is_some() {
            return Err("managed stream contains a provider error".into());
        }
        if let Some(u) = value.get("usage").filter(|u| !u.is_null()) {
            // Observe only fixed known fields before any semantic early return.
            let snapshot = observe_managed_usage(u, if usage.is_some() { 2 } else { 1 });
            if let Some((_, first)) = &usage {
                return Err(CompletionValidationFailure::usage(
                    "managed stream repeated its usage receipt",
                    ManagedProviderDiagnosticKind::UsageInconsistent,
                    ManagedUsageRejectionKind::RepeatedReceipt,
                )
                .with_observations(vec![first.clone(), snapshot]));
            }
            let parsed = parse_managed_usage(u).map_err(|failure| {
                CompletionValidationFailure::from(failure).with_observations(vec![snapshot.clone()])
            })?;
            usage = Some((parsed, snapshot));
        }
        let choices = value["choices"]
            .as_array()
            .ok_or("managed choices are malformed")?;
        if choices.len() > 1 {
            return Err("managed stream returned multiple choices".into());
        }
        for choice in choices {
            if finished || choice["index"].as_u64() != Some(0) {
                return Err("managed stream continued after its finish reason".into());
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                if !matches!(reason, "stop" | "tool_calls") {
                    return Err("managed response did not complete within its bounds".into());
                }
                finished = true;
            }
            for tool in choice["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let index = tool["index"]
                    .as_u64()
                    .ok_or("managed tool call is malformed")?;
                let entry = tools.entry(index).or_default();
                if let Some(name) = tool["function"]["name"].as_str() {
                    entry.0.push_str(name);
                }
                if let Some(args) = tool["function"]["arguments"].as_str() {
                    entry.1.push_str(args);
                }
                if entry.0.len() > 32
                    || entry.1.len() > 16 * 1024
                    || tools.len() > MAX_TOOL_CALLS as usize
                {
                    return Err("managed tool arguments exceed their bounds".into());
                }
            }
        }
    }
    if !finished || !done {
        return Err("managed stream did not prove a completed response".into());
    }
    let (usage, snapshot) = usage.ok_or_else(|| {
        CompletionValidationFailure::usage(
            "managed stream usage is missing",
            ManagedProviderDiagnosticKind::UsageMissing,
            ManagedUsageRejectionKind::MissingReceipt,
        )
    })?;
    for (name, arguments) in tools.values() {
        if !matches!(
            name.as_str(),
            "read_file"
                | "write"
                | "search_replace"
                | "list_dir"
                | "grep"
                | "search_tool"
                | "use_tool"
        ) || serde_json::from_str::<Value>(arguments)
            .ok()
            .is_none_or(|value| !value.is_object())
        {
            return Err("managed response requested an unsupported tool".into());
        }
    }
    Ok((tools.len() as u32, usage, snapshot))
}

#[cfg(all(test, target_os = "macos"))]
#[path = "managed_provider_offline_tests.rs"]
mod offline_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Home;
    impl Drop for Home {
        fn drop(&mut self) {
            crate::set_grokptah_home_override(None);
        }
    }

    pub(super) fn credentials() -> crate::auth_store::WireCredentials {
        crate::auth_store::WireCredentials {
            provider_id: "xai".into(),
            bearer: "upstream-test-secret-never-child".into(),
            oidc_token_auth: false,
            display_name: "test".into(),
            method: "api_key".into(),
            user_id: None,
            team_id: None,
            auth_scope: None,
            refresh_token: None,
            oidc_issuer: None,
            oidc_client_id: None,
            principal_type: None,
            principal_id: None,
            expires_at: None,
        }
    }

    fn model_target(base_url: &str) -> crate::host_helpers::ResolvedModelTarget {
        crate::host_helpers::ResolvedModelTarget {
            base_url: base_url.into(),
            wire_model: "grok-build-0.1".into(),
            dialect: crate::gateway_config::ProviderDialect::XaiChatCompletions,
            capabilities: crate::gateway_config::ModelCapabilities {
                tools: true,
                stream: true,
                ..Default::default()
            },
            deadline_class: crate::gateway_config::ProviderDeadlineClass::Standard,
        }
    }

    #[test]
    fn official_proxy_managed_request_binds_host_model_override() {
        let target = model_target("https://cli-chat-proxy.grok.com/v1");
        let mut creds = credentials();
        creds.oidc_token_auth = true;
        let body = request(0);
        let req = bind_official_proxy_model_override(
            reqwest::Client::new().post("https://cli-chat-proxy.grok.com/v1/chat/completions"),
            &target,
            &creds,
            &body,
        )
        .unwrap()
        .json(&body)
        .build()
        .unwrap();
        assert_eq!(
            req.headers()["x-grok-model-override"],
            body["model"].as_str().unwrap()
        );
    }

    #[test]
    fn model_override_and_body_model_must_match() {
        let target = model_target("https://cli-chat-proxy.grok.com/v1");
        let mut creds = credentials();
        creds.oidc_token_auth = true;
        let mut body = request(0);
        body["model"] = json!("different-model");
        assert!(bind_official_proxy_model_override(
            reqwest::Client::new().post("https://cli-chat-proxy.grok.com/v1/chat/completions"),
            &target,
            &creds,
            &body,
        )
        .is_err());
    }

    #[test]
    fn non_proxy_routes_do_not_receive_cli_proxy_override() {
        let body = request(0);
        for (base, oidc) in [
            ("https://api.x.ai/v1", true),
            ("https://compatible.example/v1", true),
            ("https://cli-chat-proxy.grok.com/v1", false),
        ] {
            let target = model_target(base);
            let mut creds = credentials();
            creds.oidc_token_auth = oidc;
            let req = bind_official_proxy_model_override(
                reqwest::Client::new().post(format!("{base}/chat/completions")),
                &target,
                &creds,
                &body,
            )
            .unwrap()
            .build()
            .unwrap();
            assert!(!req.headers().contains_key("x-grok-model-override"));
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn child_cannot_supply_or_change_model_override() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|headers: HeaderMap| async move {
                assert!(!headers.contains_key("x-grok-model-override"));
                ([("content-type", "text/event-stream")], stream("ok"))
            }),
        );
        let (relay, server) = local_fixture(root.path().join("leases"), router).await;
        let (_id, secret) = issue(&relay, "child-override");
        let response = reqwest::Client::new()
            .post(format!("{}/chat/completions", relay.policy.endpoint))
            .bearer_auth(secret)
            .header("x-grok-model-override", "child-chosen-model")
            .json(&request(0))
            // authority-allow-unauthenticated-wire: Test-only local child
            // request; the relay retains canonical upstream send authority.
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        server.abort();
    }

    fn stream(text: &str) -> String {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"id":"test", "object":"chat.completion.chunk", "created":0, "model":"grok-build-0.1", "choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":"stop"}]}),
            json!({"id":"test", "object":"chat.completion.chunk", "created":0, "model":"grok-build-0.1", "choices":[], "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}})
        )
    }

    fn accounting_stream(usage: Value) -> String {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"offline"},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":usage})
        )
    }

    #[allow(clippy::await_holding_lock)]
    async fn accounting_fixture(usage: Value) -> (StatusCode, Value, u32, usize) {
        accounting_fixture_with_limit(usage, None).await
    }

    #[allow(clippy::await_holding_lock)]
    async fn accounting_fixture_with_limit(
        usage: Value,
        limit: Option<u64>,
    ) -> (StatusCode, Value, u32, usize) {
        accounting_fixture_body(accounting_stream(usage), limit).await
    }

    #[allow(clippy::await_holding_lock)]
    async fn accounting_fixture_body(
        body: String,
        limit: Option<u64>,
    ) -> (StatusCode, Value, u32, usize) {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let calls = Arc::new(AtomicU32::new(0));
        let observed = calls.clone();
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let observed = observed.clone();
                let body = body.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        );
        let (relay, server) = local_fixture(root.path().join("leases"), router).await;
        let (id, secret) = issue(&relay, "accounting-fixture");
        if let Some(limit) = limit {
            relay
                .state
                .leases
                .lock()
                .unwrap()
                .get_mut(&id)
                .unwrap()
                .max_total_tokens = limit;
        }
        let status = send(&relay, &secret, &request(0)).await.status();
        let evidence = serde_json::to_value(relay.evidence(&id).unwrap()).unwrap();
        if status != StatusCode::OK {
            assert_eq!(
                send(&relay, &secret, &request(1)).await.status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let count = calls.load(Ordering::SeqCst);
        let custody =
            fs::read_to_string(root.path().join("host/authority/provider-send-v1.key")).unwrap();
        let operator =
            crate::provider_transport::authenticate_provider_reconciliation(&custody).unwrap();
        let pending =
            crate::provider_transport::provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len();
        server.abort();
        (status, evidence, count, pending)
    }

    #[tokio::test]
    async fn combined_reasoning_cache_and_exact_cost_survive_canonical_forward() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_tokens_details":{"text_tokens":100,"audio_tokens":0,"image_tokens":0,"cached_tokens":20},
            "completion_tokens_details":{"reasoning_tokens":3,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0},
            "cost_in_usd_ticks":777,"num_sources_used":0
        })).await;
        assert_eq!((status, calls, pending), (StatusCode::OK, 1, 0));
        assert_eq!(evidence["reasoningTokens"], 3);
        assert_eq!(evidence["cacheReadInputTokens"], 20);
        assert_eq!(evidence["totalTokens"], 110);
        assert_eq!(evidence["costInUsdTicks"], 777);
        assert_eq!(evidence["accountingComplete"], true);
    }

    #[tokio::test]
    async fn unknown_additional_usage_cannot_settle_as_complete() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "additional_charge_ticks":123
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["accountingComplete"], false);
        assert_eq!(evidence["responsesCompleted"], 0);
    }

    #[tokio::test]
    async fn cache_detail_larger_than_prompt_fails_closed() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_tokens_details":{"cached_tokens":101}
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["accountingComplete"], false);
    }

    #[tokio::test]
    async fn reasoning_detail_larger_than_completion_fails_closed() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "completion_tokens_details":{"reasoning_tokens":11}
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["accountingComplete"], false);
    }

    #[tokio::test]
    async fn zero_valued_usage_details_keep_cost_unknown_not_free() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_tokens_details":{"text_tokens":100,"cached_tokens":0,"audio_tokens":0,"image_tokens":0},
            "completion_tokens_details":{"reasoning_tokens":0,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0},
            "num_sources_used":0
        })).await;
        assert_eq!((status, calls, pending), (StatusCode::OK, 1, 0));
        assert_eq!(evidence["totalTokens"], 110);
        assert_eq!(evidence["cacheReadInputTokens"], 0);
        assert_eq!(evidence["reasoningTokens"], 0);
        assert_eq!(evidence["costMissingCalls"], 1);
        assert_eq!(evidence["costComplete"], false);
    }

    #[tokio::test]
    async fn additive_reasoning_total_is_not_a_free_output_bypass() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":115,
            "completion_tokens_details":{"reasoning_tokens":5}
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["accountingComplete"], false);
    }

    #[tokio::test]
    async fn rejected_total_retains_known_sibling_observations_at_relay_boundary() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":3500,"completion_tokens":114,"total_tokens":3946,
            "prompt_tokens_details":{"text_tokens":3500,"audio_tokens":0,"image_tokens":0,"cached_tokens":240},
            "completion_tokens_details":{"reasoning_tokens":332,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0},
            "num_sources_used":0,"cost_in_usd_ticks":1665
        })).await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["responsesCompleted"], 0);
        assert_eq!(evidence["accountingComplete"], false);
        assert_eq!(evidence["costComplete"], false);
        let fields = evidence["usageObservation"]["snapshots"][0]["fields"]
            .as_array()
            .expect("rejected receipt must retain an allowlisted snapshot");
        let observed = |path: &str| fields.iter().find(|field| field["path"] == path).unwrap();
        assert_eq!(observed("prompt_tokens")["value"], 3500);
        assert_eq!(observed("completion_tokens")["value"], 114);
        assert_eq!(observed("total_tokens")["value"], 3946);
        assert_eq!(
            observed("prompt_tokens_details.cached_tokens")["value"],
            240
        );
        assert_eq!(
            observed("completion_tokens_details.reasoning_tokens")["value"],
            332
        );
        assert_eq!(observed("cost_in_usd_ticks")["value"], 1665);
        assert_eq!(
            evidence["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["kind"] == "usage_inconsistent")
                .unwrap()["usageRejection"]["kind"],
            "conflicting_total"
        );
    }

    fn observed_field<'a>(evidence: &'a Value, path: &str) -> &'a Value {
        evidence["usageObservation"]["snapshots"][0]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["path"] == path)
            .unwrap()
    }

    #[tokio::test]
    async fn accepted_receipts_retain_distinct_missing_zero_subset_and_cost_observations() {
        let (status, ordinary, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::OK, 1, 0));
        assert_eq!(
            ordinary["usageObservation"]["snapshots"][0]["receiptState"]["state"],
            "object"
        );
        assert_eq!(
            ordinary["usageObservation"]["snapshots"][0]["promptDetailsState"]["state"],
            "missing"
        );
        assert_eq!(
            observed_field(&ordinary, "cost_in_usd_ticks")["state"],
            "missing"
        );
        assert_eq!(
            observed_field(&ordinary, "completion_tokens_details.reasoning_tokens")["state"],
            "parent_unavailable"
        );
        assert_eq!(ordinary["costComplete"], false);

        let (status, combined, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_tokens_details":{"text_tokens":100,"audio_tokens":0,"image_tokens":0,"cached_tokens":20},
            "completion_tokens_details":{"reasoning_tokens":3,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0},
            "num_sources_used":0,"cost_in_usd_ticks":777
        })).await;
        assert_eq!((status, calls, pending), (StatusCode::OK, 1, 0));
        assert_eq!(combined["totalTokens"], 110);
        assert_eq!(combined["cacheReadInputTokens"], 20);
        assert_eq!(combined["reasoningTokens"], 3);
        assert_eq!(combined["costInUsdTicks"], 777);
        assert_eq!(
            observed_field(&combined, "prompt_tokens_details.audio_tokens")["state"],
            "zero"
        );
        assert_eq!(
            observed_field(&combined, "completion_tokens_details.audio_tokens")["state"],
            "zero"
        );
        assert_eq!(
            observed_field(&combined, "completion_tokens_details.reasoning_tokens")["value"],
            3
        );
        assert_eq!(observed_field(&combined, "cost_in_usd_ticks")["value"], 777);
    }

    #[tokio::test]
    async fn total_mismatch_preserves_reasoning_missing_and_malformed_siblings() {
        for (details, state, value) in [
            (json!({"reasoning_tokens":5}), "positive", Some(5)),
            (json!({}), "missing", None),
        ] {
            let (status, evidence, calls, pending) = accounting_fixture(json!({
                "prompt_tokens":100,"completion_tokens":10,"total_tokens":116,
                "completion_tokens_details":details,"cost_in_usd_ticks":2000
            }))
            .await;
            assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
            assert_eq!(
                evidence["diagnostics"][1]["usageRejection"]["kind"],
                "conflicting_total"
            );
            assert_eq!(
                observed_field(&evidence, "completion_tokens_details.reasoning_tokens")["state"],
                state
            );
            assert_eq!(
                observed_field(&evidence, "completion_tokens_details.reasoning_tokens")["value"]
                    .as_i64(),
                value
            );
            assert_eq!(
                observed_field(&evidence, "cost_in_usd_ticks")["value"],
                2000
            );
            assert_eq!(evidence["responsesCompleted"], 0);
            assert_eq!(evidence["revoked"], true);
        }

        let (status, malformed, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":115,
            "prompt_tokens_details":"SECRET_DETAIL_CANARY",
            "completion_tokens_details":{"reasoning_tokens":5},
            "cost_in_usd_ticks":1665
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(
            malformed["diagnostics"][1]["usageRejection"]["kind"],
            "conflicting_total"
        );
        assert_eq!(
            malformed["usageObservation"]["snapshots"][0]["promptDetailsState"]["state"],
            "wrong_type"
        );
        assert_eq!(
            malformed["usageObservation"]["snapshots"][0]["promptDetailsState"]
                ["contentSuppressed"],
            true
        );
        assert_eq!(
            observed_field(&malformed, "prompt_tokens_details.cached_tokens")["state"],
            "parent_unavailable"
        );
        assert_eq!(
            observed_field(&malformed, "completion_tokens_details.reasoning_tokens")["value"],
            5
        );
        assert_eq!(
            observed_field(&malformed, "cost_in_usd_ticks")["value"],
            1665
        );
        assert!(!malformed.to_string().contains("SECRET_DETAIL_CANARY"));
    }

    #[tokio::test]
    async fn total_mismatch_preserves_cost_states_without_accepting_cost() {
        let cases = [
            (None, "missing", None, false),
            (Some(Value::Null), "null", None, false),
            (Some(json!(0)), "zero", Some(0), false),
            (Some(json!(1665)), "positive", Some(1665), false),
            (Some(json!(-7)), "negative", Some(-7), false),
            (Some(json!(1.5)), "non_integral", None, false),
            (Some(json!("SECRET_COST_CANARY")), "wrong_type", None, true),
        ];
        for (cost, state, value, suppressed) in cases {
            let mut usage = json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":115});
            if let Some(cost) = cost {
                usage["cost_in_usd_ticks"] = cost;
            }
            let (status, evidence, calls, pending) = accounting_fixture(usage).await;
            assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
            assert_eq!(
                evidence["diagnostics"][1]["usageRejection"]["kind"],
                "conflicting_total"
            );
            let cost = observed_field(&evidence, "cost_in_usd_ticks");
            assert_eq!(cost["state"], state);
            assert_eq!(cost["value"].as_i64(), value);
            assert_eq!(cost["contentSuppressed"], suppressed);
            if state == "non_integral" {
                assert_eq!(cost["nonIntegralValue"], 1.5);
                assert_eq!(cost["valueOmitted"], false);
            }
            assert_eq!(evidence["costComplete"], false);
            assert!(evidence["costInUsdTicks"].is_null());
            assert!(!evidence.to_string().contains("SECRET_COST_CANARY"));
        }
    }

    #[tokio::test]
    async fn other_early_rejections_keep_safely_inspectable_sibling_fields() {
        let cases = [
            (
                json!({"prompt_tokens":"SECRET_FIELD_CANARY","completion_tokens":10,"total_tokens":110,"cost_in_usd_ticks":1665}),
                "malformed_field",
                "completion_tokens",
                10,
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":{"cached_tokens":2,"SECRET_UNKNOWN_CANARY":"SECRET_VALUE_CANARY"},"completion_tokens_details":{"reasoning_tokens":3},"cost_in_usd_ticks":1665}),
                "unsupported_details_field",
                "completion_tokens_details.reasoning_tokens",
                3,
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":{"cached_tokens":101},"completion_tokens_details":{"reasoning_tokens":3},"cost_in_usd_ticks":1665}),
                "conflicting_subset",
                "completion_tokens_details.reasoning_tokens",
                3,
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":{"cached_tokens":2},"completion_tokens_details":{"reasoning_tokens":3},"cost_in_usd_ticks":null}),
                "cost_null",
                "completion_tokens_details.reasoning_tokens",
                3,
            ),
        ];
        for (usage, expected_rejection, sibling_path, sibling_value) in cases {
            let (status, evidence, calls, pending) = accounting_fixture(usage).await;
            assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
            assert_eq!(
                evidence["diagnostics"][1]["usageRejection"]["kind"],
                expected_rejection
            );
            assert_eq!(
                observed_field(&evidence, sibling_path)["value"],
                sibling_value
            );
            assert_eq!(
                observed_field(&evidence, "cost_in_usd_ticks")["state"],
                if expected_rejection == "cost_null" {
                    "null"
                } else {
                    "positive"
                }
            );
            assert_eq!(evidence["responsesCompleted"], 0);
            assert_eq!(evidence["revoked"], true);
            assert!(!evidence.to_string().contains("SECRET_"));
        }
    }

    #[tokio::test]
    async fn unknown_keys_and_large_numbers_have_bounded_independent_observations() {
        let (status, unknown, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "prompt_tokens_details":{"cached_tokens":3,"SECRET_NESTED_CANARY":"SECRET_VALUE_CANARY"},
            "completion_tokens_details":{"reasoning_tokens":2,"SECRET_OTHER_CANARY":"SECRET_VALUE_CANARY"},
            "SECRET_TOP_CANARY":"SECRET_VALUE_CANARY"
        })).await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(
            unknown["diagnostics"][1]["usageRejection"]["kind"],
            "unsupported_field"
        );
        let snapshot = &unknown["usageObservation"]["snapshots"][0];
        assert_eq!(snapshot["unknownTopLevelKeys"]["countUpToFour"], 1);
        assert_eq!(snapshot["unknownPromptDetailsKeys"]["countUpToFour"], 1);
        assert_eq!(snapshot["unknownCompletionDetailsKeys"]["countUpToFour"], 1);
        assert_eq!(snapshot["unknownTopLevelKeys"]["contentSuppressed"], true);
        assert_eq!(
            observed_field(&unknown, "prompt_tokens_details.cached_tokens")["value"],
            3
        );
        assert_eq!(
            observed_field(&unknown, "completion_tokens_details.reasoning_tokens")["value"],
            2
        );
        assert!(!unknown.to_string().contains("SECRET_"));

        let (status, large, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":u64::MAX,"completion_tokens":10,"total_tokens":u64::MAX,
            "cost_in_usd_ticks":u64::MAX
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(
            large["diagnostics"][1]["usageRejection"]["kind"],
            "token_bound_exceeded"
        );
        for path in ["prompt_tokens", "total_tokens", "cost_in_usd_ticks"] {
            let field = observed_field(&large, path);
            assert_eq!(field["state"], "positive");
            assert_eq!(field["valueOmitted"], true);
            assert!(field["value"].is_null());
        }
        assert_eq!(observed_field(&large, "completion_tokens")["value"], 10);
    }

    #[tokio::test]
    async fn credential_echo_suppresses_the_entire_usage_snapshot() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":115,
            "completion_tokens_details":{"reasoning_tokens":"upstream-test-secret-never-child"}
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["usageObservation"]["credentialSuppressed"], true);
        assert_eq!(
            evidence["usageObservation"]["snapshots"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(evidence["responsesCompleted"], 0);
        assert!(!evidence
            .to_string()
            .contains("upstream-test-secret-never-child"));
    }

    #[tokio::test]
    async fn repeated_receipts_keep_two_bounded_snapshots_and_original_rejection() {
        let first = json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110});
        let second = json!({"prompt_tokens":101,"completion_tokens":10,"total_tokens":111,"cost_in_usd_ticks":1665});
        let second_line = format!(
            "data: {}\n\ndata: [DONE]",
            json!({"choices":[],"usage":second})
        );
        let body = accounting_stream(first).replace("data: [DONE]", &second_line);
        let (status, evidence, calls, pending) = accounting_fixture_body(body, None).await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(
            evidence["diagnostics"][1]["usageRejection"]["kind"],
            "repeated_receipt"
        );
        assert_eq!(
            evidence["usageObservation"]["snapshots"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            evidence["usageObservation"]["snapshots"][0]["receiptOrdinal"],
            1
        );
        assert_eq!(
            evidence["usageObservation"]["snapshots"][1]["receiptOrdinal"],
            2
        );
        assert_eq!(
            evidence["usageObservation"]["snapshots"][1]["fields"][0]["value"],
            101
        );
        assert_eq!(evidence["usageObservation"]["observationsOmitted"], false);
        assert_eq!(evidence["responsesCompleted"], 0);
    }

    #[test]
    fn usage_snapshot_and_diagnostic_counts_are_explicitly_bounded() {
        let sample = observe_managed_usage(
            &json!({
                "prompt_tokens":65536,"completion_tokens":1024,"total_tokens":65536,
                "prompt_tokens_details":{"text_tokens":65536,"audio_tokens":0,"image_tokens":0,"cached_tokens":65536},
                "completion_tokens_details":{"reasoning_tokens":1024,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0},
                "num_sources_used":0,"cost_in_usd_ticks":1_000_000_000_000_i64
            }),
            1,
        );
        assert_eq!(sample.fields.len(), 13);
        assert!(serde_json::to_vec(&sample).unwrap().len() <= 3072);
        let mut observation = ManagedUsageObservationEvidence::default();
        observation.retain([sample.clone(), sample.clone(), sample]);
        assert_eq!(observation.snapshots.len(), 2);
        assert!(observation.observations_omitted);
        assert!(serde_json::to_vec(&observation).unwrap().len() <= 6144);
        let mut evidence = ManagedProviderEvidence::default();
        for _ in 0..33 {
            evidence.record_diagnostic(ManagedProviderDiagnosticKind::UsageInconsistent);
        }
        assert_eq!(evidence.diagnostics.len(), 32);
        assert!(evidence.diagnostics_truncated);
        let mut many_unknown = json!({"prompt_tokens":1,"completion_tokens":1,"total_tokens":2});
        for index in 0..6 {
            many_unknown[format!("secret_unknown_{index}")] = json!("SECRET_VALUE_CANARY");
        }
        let snapshot = observe_managed_usage(&many_unknown, 1);
        let unknown = snapshot.unknown_top_level_keys.unwrap();
        assert_eq!(unknown.count_up_to_four, 4);
        assert!(unknown.more && unknown.content_suppressed);
        assert!(!serde_json::to_string(&snapshot)
            .unwrap()
            .contains("secret_unknown"));
    }

    #[tokio::test]
    async fn oversized_or_zero_cost_tick_values_fail_closed() {
        for ticks in [json!(0), json!(u64::MAX)] {
            let (status, evidence, calls, pending) = accounting_fixture(json!({
                "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
                "cost_in_usd_ticks":ticks
            }))
            .await;
            assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
            assert_eq!(evidence["accountingComplete"], false);
        }
    }

    #[tokio::test]
    async fn reported_cache_cannot_increase_remaining_token_authority() {
        let (status, evidence, calls, pending) = accounting_fixture_with_limit(
            json!({
                "prompt_tokens":1000,"completion_tokens":400,"total_tokens":1400,
                "prompt_tokens_details":{"cached_tokens":900},
                "completion_tokens_details":{"reasoning_tokens":300}
            }),
            Some(1300),
        )
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["accountingComplete"], false);
        assert_eq!(evidence["responsesCompleted"], 0);
    }

    #[tokio::test]
    async fn server_side_usage_cannot_hide_an_additional_charge() {
        let (status, evidence, calls, pending) = accounting_fixture(json!({
            "prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
            "num_sources_used":1,"cost_in_usd_ticks":777
        }))
        .await;
        assert_eq!((status, calls, pending), (StatusCode::BAD_GATEWAY, 1, 1));
        assert_eq!(evidence["accountingComplete"], false);
    }

    #[allow(clippy::await_holding_lock)]
    async fn assert_second_cost_fails_closed(second_cost: Option<i64>) {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let calls = Arc::new(AtomicU32::new(0));
        let observed = calls.clone();
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let observed = observed.clone();
                async move {
                    let index = observed.fetch_add(1, Ordering::SeqCst);
                    let mut usage =
                        json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110});
                    if index == 0 {
                        usage["cost_in_usd_ticks"] =
                            json!(if second_cost.is_some() { i64::MAX } else { 777 });
                    } else if let Some(ticks) = second_cost {
                        usage["cost_in_usd_ticks"] = json!(ticks);
                    }
                    (
                        [("content-type", "text/event-stream")],
                        accounting_stream(usage),
                    )
                }
            }),
        );
        let (relay, server) = local_fixture(root.path().join("leases"), router).await;
        let (id, secret) = issue(&relay, "cost-aggregation");
        assert_eq!(
            send(&relay, &secret, &request(0)).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&relay, &secret, &request(1)).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            send(&relay, &secret, &request(2)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let evidence = relay.evidence(&id).unwrap();
        assert_eq!(
            (
                evidence.requests_reserved,
                evidence.wire_attempts,
                evidence.responses_completed
            ),
            (2, 2, 1)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(evidence.total_tokens, Some(110));
        assert_eq!(evidence.input_tokens, 100);
        assert_eq!(evidence.output_tokens, 10);
        assert_eq!(
            evidence.cost_in_usd_ticks,
            Some(if second_cost.is_some() { i64::MAX } else { 777 })
        );
        assert!(!evidence.accounting_complete && !evidence.cost_complete);
        assert!(evidence.uncertain && evidence.revoked);
        assert_eq!(
            evidence.interruption,
            Some(ManagedProviderDiagnosticKind::UsageInconsistent)
        );
        let custody =
            fs::read_to_string(root.path().join("host/authority/provider-send-v1.key")).unwrap();
        let operator =
            crate::provider_transport::authenticate_provider_reconciliation(&custody).unwrap();
        assert_eq!(
            crate::provider_transport::provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn mixed_reported_and_missing_cost_cannot_be_marked_complete() {
        assert_second_cost_fails_closed(None).await;
    }

    #[tokio::test]
    async fn exact_cost_accumulation_overflow_cannot_be_marked_complete() {
        assert_second_cost_fails_closed(Some(1)).await;
    }

    #[test]
    fn historical_unknown_total_is_never_reused_as_zero_budget() {
        let prior = ManagedProviderEvidence {
            input_tokens: 100,
            output_tokens: 10,
            usage_observed: true,
            total_tokens: None,
            ..Default::default()
        };
        assert_eq!(prior.accounted_total_tokens(), None);
        assert!(prior
            .checked_accounting_after(ManagedUsage {
                input_tokens: 100,
                output_tokens: 10,
                total_tokens: 110,
                cache_read_input_tokens: 0,
                reasoning_tokens: 0,
                cost_in_usd_ticks: None,
            })
            .is_err());
        assert_eq!(
            ManagedProviderEvidence::default().accounted_total_tokens(),
            Some(0)
        );
        let malformed = ManagedProviderEvidence {
            input_tokens: 1,
            ..Default::default()
        };
        assert_eq!(malformed.accounted_total_tokens(), None);
    }

    async fn relay(
        dir: PathBuf,
        status: StatusCode,
    ) -> (
        Arc<ManagedProviderRelay>,
        Arc<AtomicU32>,
        tokio::task::JoinHandle<()>,
    ) {
        let calls = Arc::new(AtomicU32::new(0));
        let count = calls.clone();
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let count = count.clone();
                async move {
                    assert_eq!(
                        headers["authorization"],
                        "Bearer upstream-test-secret-never-child"
                    );
                    assert!(headers.contains_key("idempotency-key"));
                    assert_eq!(body["max_tokens"], MAX_OUTPUT_TOKENS);
                    count.fetch_add(1, Ordering::SeqCst);
                    if status == StatusCode::REQUEST_TIMEOUT {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    (
                        status,
                        [("content-type", "text/event-stream")],
                        stream(
                            "Offline protocol probe completed.\nGROK_BUILD_VERDICT=not_complete",
                        ),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let target = crate::host_helpers::ResolvedModelTarget {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            wire_model: "grok-build-0.1".into(),
            dialect: crate::gateway_config::ProviderDialect::XaiChatCompletions,
            capabilities: crate::gateway_config::ModelCapabilities {
                tools: true,
                stream: true,
                ..Default::default()
            },
            deadline_class: crate::gateway_config::ProviderDeadlineClass::Standard,
        };
        (
            ManagedProviderRelay::with_target(credentials(), target, dir)
                .await
                .unwrap(),
            calls,
            server,
        )
    }

    pub(super) async fn local_fixture(
        dir: PathBuf,
        router: Router,
    ) -> (Arc<ManagedProviderRelay>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let target = crate::host_helpers::ResolvedModelTarget {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            wire_model: "grok-build-0.1".into(),
            dialect: crate::gateway_config::ProviderDialect::XaiChatCompletions,
            capabilities: crate::gateway_config::ModelCapabilities {
                tools: true,
                stream: true,
                ..Default::default()
            },
            deadline_class: crate::gateway_config::ProviderDeadlineClass::Standard,
        };
        (
            ManagedProviderRelay::with_target(credentials(), target, dir)
                .await
                .unwrap(),
            server,
        )
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn typed_http_and_protocol_diagnostics_are_bounded_and_secret_free() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        for (index, status, body, expected) in [
            (0, StatusCode::BAD_REQUEST, json!({"error":{"type":"invalid_request_error","code":"invalid_request_error","message":"SECRET_RESPONSE_CANARY"}}).to_string(), ManagedProviderDiagnosticKind::HttpRejected),
            (1, StatusCode::UNAUTHORIZED, json!({"error":{"type":"authentication_error","code":"invalid_api_key","message":"SECRET_RESPONSE_CANARY"}}).to_string(), ManagedProviderDiagnosticKind::HttpRejected),
            (2, StatusCode::FORBIDDEN, json!({"error":{"code":"permission_denied","message":"SECRET_RESPONSE_CANARY"}}).to_string(), ManagedProviderDiagnosticKind::HttpRejected),
            (3, StatusCode::TOO_MANY_REQUESTS, json!({"error":{"code":"rate_limit_exceeded","message":"SECRET_RESPONSE_CANARY"}}).to_string(), ManagedProviderDiagnosticKind::HttpRejected),
            (4, StatusCode::SERVICE_UNAVAILABLE, json!({"error":{"code":"server_error","message":"SECRET_RESPONSE_CANARY"}}).to_string(), ManagedProviderDiagnosticKind::HttpRejected),
            (5, StatusCode::OK, "data: SECRET_RESPONSE_CANARY\n\ndata: [DONE]\n\n".into(), ManagedProviderDiagnosticKind::ProtocolFailure),
            (6, StatusCode::OK, stream("ok").replace("\"usage\":", "\"missing_usage\":"), ManagedProviderDiagnosticKind::UsageMissing),
            (7, StatusCode::OK, stream("ok").replace("\"total_tokens\":110", "\"total_tokens\":111"), ManagedProviderDiagnosticKind::UsageInconsistent),
            (8, StatusCode::OK, stream("ok"), ManagedProviderDiagnosticKind::Completed),
            (9, StatusCode::BAD_REQUEST, json!({"error":{"type":"SECRET_RESPONSE_CANARY","code":"SECRET_RESPONSE_CANARY"}}).to_string(), ManagedProviderDiagnosticKind::HttpRejected),
        ] {
            let calls = Arc::new(AtomicU32::new(0));
            let count = calls.clone();
            let router = Router::new().route("/v1/chat/completions", post(move || {
                let count = count.clone(); let body = body.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    (status, [("content-type", "text/event-stream"), ("x-request-id", "12345678-1234-4234-8234-123456789abc"), ("set-cookie", "COOKIE_CANARY")], body)
                }
            }));
            let (relay, server) = local_fixture(root.path().join(format!("leases-{index}")), router).await;
            let (id, secret) = issue(&relay, "diagnostics");
            let response = send(&relay, &secret, &request(index)).await;
            assert_eq!(response.status(), if expected == ManagedProviderDiagnosticKind::Completed { StatusCode::OK } else { StatusCode::BAD_GATEWAY });
            let e = relay.evidence(&id).unwrap();
            assert_eq!((e.wire_attempts, e.http_responses_observed), (1, 1));
            assert!(e.diagnostics.iter().any(|d| d.kind == expected), "{e:?}");
            assert!(e.diagnostics.iter().any(|d| d.http_status == Some(status.as_u16())));
            assert_eq!(e.responses_completed, u32::from(expected == ManagedProviderDiagnosticKind::Completed));
            let encoded = serde_json::to_string(&e).unwrap();
            for canary in ["SECRET_RESPONSE_CANARY", "COOKIE_CANARY", &secret, &credentials().bearer] { assert!(!encoded.contains(canary)); }
            if index == 1 { assert!(e.diagnostics.iter().any(|d| d.provider_error_code == Some(ProviderMachineError::InvalidApiKey))); }
            if index == 9 { assert!(e.diagnostics.iter().all(|d| d.provider_error_code.is_none() && d.provider_error_type.is_none())); }
            if expected != ManagedProviderDiagnosticKind::Completed {
                assert!(e.revoked && e.uncertain && !e.accounting_complete);
                assert_eq!(send(&relay, &secret, &request(100)).await.status(), StatusCode::UNAUTHORIZED);
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "SECRET_HEADER_CANARY".parse().unwrap());
        assert!(safe_request_id(&headers, "child", "upstream").is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn relay_connection_disconnect_never_permits_an_additional_forward() {
        use tokio::io::AsyncWriteExt;
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let calls = Arc::new(AtomicU32::new(0));
        let count = calls.clone();
        let observed = Arc::new(tokio::sync::Notify::new());
        let barrier = observed.clone();
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let count = count.clone();
                let barrier = barrier.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    barrier.notify_one();
                    std::future::pending::<Response>().await
                }
            }),
        );
        let (relay, server) = local_fixture(root.path().join("leases"), router).await;
        let (id, secret) = issue(&relay, "disconnect");
        let body = serde_json::to_vec(&request(0)).unwrap();
        let mut connection = tokio::net::TcpStream::connect(("127.0.0.1", relay.policy.port))
            .await
            .unwrap();
        let prefix = format!("POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len());
        connection.write_all(prefix.as_bytes()).await.unwrap();
        connection.write_all(&body).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), observed.notified())
            .await
            .unwrap();
        drop(connection);
        tokio::time::sleep(Duration::from_millis(30)).await;
        let before = relay.evidence(&id).unwrap();
        let status = send(&relay, &secret, &request(1)).await.status();
        assert!(matches!(
            status,
            StatusCode::UNAUTHORIZED | StatusCode::BAD_GATEWAY
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while !relay.provider_quiescent(&id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let after = relay.evidence(&id).unwrap();
        assert!(after.revoked && after.uncertain && !after.accounting_complete);
        assert_eq!(
            (
                after.requests_reserved,
                after.wire_attempts,
                calls.load(Ordering::SeqCst)
            ),
            (1, 1, 1)
        );
        // Hyper may retain a handler after disconnect; while retained, admission
        // remains locked out. If it drops the handler, the settlement guard
        // revokes immediately. Neither path can spend a changed request.
        assert!(before.revoked || before.responses_completed == 0);
        server.abort();
    }

    fn issue(relay: &ManagedProviderRelay, request: &str) -> (String, String) {
        let id = relay.lease_id_for_request("assignment", request).unwrap();
        let handle = relay.resolve(&id).unwrap();
        assert!(!format!("{handle:?}").contains("gptah-"));
        assert!(!format!("{relay:?}").contains("upstream-test-secret"));
        let secret = relay.state.leases.lock().unwrap()[&id].secret.clone();
        assert_eq!(fs::read(relay.credential_dir.join(&id)).unwrap(), b"{}");
        (id, secret)
    }

    fn request(index: u32) -> Value {
        json!({"model":"grok-build-0.1","stream":true,"messages":[{"role":"user","content":format!("bounded task {index}")}],
            "tools":[{"type":"function","function":{"name":"write","parameters":{"type":"object"}}}],"max_tokens":99999})
    }

    async fn send(relay: &ManagedProviderRelay, secret: &str, body: &Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{}/chat/completions", relay.policy.endpoint))
            .bearer_auth(secret)
            .json(body)
            // authority-allow-unauthenticated-wire: Test-only local relay
            // capability; upstream credentials use canonical send authority.
            .send()
            .await
            .unwrap()
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum HandoffCase {
        Ownership,
        Quiescence,
        ThirdRequest,
        SuccessorAbandonment,
    }

    #[allow(clippy::await_holding_lock)]
    async fn assert_completed_forward_handoff(case: HandoffCase) {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let calls = Arc::new(AtomicU32::new(0));
        let b_observed = Arc::new(tokio::sync::Notify::new());
        let release_b = Arc::new(tokio::sync::Notify::new());
        let count = calls.clone();
        let observed = b_observed.clone();
        let release = release_b.clone();
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let count = count.clone();
                let observed = observed.clone();
                let release = release.clone();
                async move {
                    assert_eq!(
                        headers["authorization"],
                        "Bearer upstream-test-secret-never-child"
                    );
                    assert!(headers.contains_key("idempotency-key"));
                    assert_eq!(body["max_tokens"], MAX_OUTPUT_TOKENS);
                    if count.fetch_add(1, Ordering::SeqCst) == 1 {
                        // B is physically observed, but has received neither
                        // headers nor usage. C (if admitted) would finish.
                        observed.notify_one();
                        release.notified().await;
                    }
                    (
                        [("content-type", "text/event-stream")],
                        stream("settled handoff turn"),
                    )
                }
            }),
        );
        let (relay, server) = local_fixture(root.path().join("leases"), router).await;
        let (id, secret) = issue(&relay, "completed-forward-handoff");
        let (published_tx, published_rx) = tokio::sync::oneshot::channel();
        let (release_a, release_rx) = tokio::sync::oneshot::channel();
        *relay.state.completed_forward_handoff.lock().unwrap() = Some(CompletedForwardHandoff {
            published: published_tx,
            release: release_rx,
        });
        let a_relay = relay.clone();
        let a_secret = secret.clone();
        let a = tokio::spawn(async move { send(&a_relay, &a_secret, &request(0)).await.status() });
        tokio::time::timeout(Duration::from_secs(3), published_rx)
            .await
            .unwrap()
            .unwrap();
        let settled_a = relay.evidence(&id).unwrap();
        assert_eq!(
            (
                settled_a.requests_reserved,
                settled_a.wire_attempts,
                settled_a.responses_completed
            ),
            (1, 1, 1)
        );
        assert!(settled_a.accounting_complete && !settled_a.revoked && !settled_a.uncertain);
        let custody =
            fs::read_to_string(root.path().join("host/authority/provider-send-v1.key")).unwrap();
        let operator =
            crate::provider_transport::authenticate_provider_reconciliation(&custody).unwrap();
        let pending = || {
            crate::provider_transport::provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len()
        };
        assert_eq!(pending(), 0, "A must be durably settled before the handoff");
        let b_relay = relay.clone();
        let b_secret = secret.clone();
        let b_id = id.clone();
        let b = tokio::spawn(async move {
            if case == HandoffCase::SuccessorAbandonment {
                // Directly own the same production forward future for a
                // deterministic task abort. HTTP disconnect is separately
                // covered; it need not drop Hyper's request handler.
                match forward(
                    &b_relay.state,
                    &b_id,
                    &serde_json::to_vec(&request(1)).unwrap(),
                )
                .await
                {
                    Ok(_) => StatusCode::OK,
                    Err(_) => StatusCode::BAD_GATEWAY,
                }
            } else {
                send(&b_relay, &b_secret, &request(1)).await.status()
            }
        });
        tokio::time::timeout(Duration::from_secs(3), b_observed.notified())
            .await
            .unwrap();
        let active_b = relay.evidence(&id).unwrap();
        assert_eq!(
            (
                active_b.requests_reserved,
                active_b.wire_attempts,
                active_b.responses_completed
            ),
            (2, 2, 1)
        );
        assert_eq!(
            pending(),
            1,
            "B must have a real outstanding canonical attempt"
        );
        assert!(relay.state.leases.lock().unwrap()[&id].in_flight);
        assert!(!relay.provider_quiescent(&id));
        release_a.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), a)
                .await
                .unwrap()
                .unwrap(),
            StatusCode::OK
        );
        let after_a_drop = relay.evidence(&id).unwrap();
        let b_still_active = relay.state.leases.lock().unwrap()[&id].in_flight;
        let quiescent = relay.provider_quiescent(&id);
        eprintln!(
            "completed-forward-handoff {}",
            json!({"case":format!("{case:?}"),"aSettled":1,"bObservedBeforeADrop":true,"bActiveAfterADrop":b_still_active,"providerQuiescentAfterADrop":quiescent,"canonicalPending":pending(),"reserved":after_a_drop.requests_reserved,"admissions":after_a_drop.wire_attempts,"fixtureCalls":calls.load(Ordering::SeqCst),"completed":after_a_drop.responses_completed,"inputTokens":after_a_drop.input_tokens,"outputTokens":after_a_drop.output_tokens})
        );
        assert_eq!(
            after_a_drop, active_b,
            "A cleanup cannot change B's evidence"
        );
        match case {
            HandoffCase::ThirdRequest => {
                let c = tokio::time::timeout(
                    Duration::from_secs(3),
                    send(&relay, &secret, &request(2)),
                )
                .await
                .unwrap();
                let after_c = relay.evidence(&id).unwrap();
                let counts = (
                    after_c.requests_reserved,
                    after_c.wire_attempts,
                    calls.load(Ordering::SeqCst),
                );
                eprintln!(
                    "completed-forward-third {}",
                    json!({"httpStatus":c.status().as_u16(),"reserved":counts.0,"admissions":counts.1,"fixtureCalls":counts.2})
                );
                assert_eq!(
                    counts,
                    (2, 2, 2),
                    "C cannot reserve or send while B owns admission"
                );
                assert_eq!(c.status(), StatusCode::BAD_GATEWAY);
                assert!(b_still_active && !quiescent);
                assert!(after_c.revoked && after_c.uncertain && !after_c.accounting_complete);
                assert!(relay.state.leases.lock().unwrap()[&id]
                    .cancelled
                    .is_cancelled());
                assert!(after_c
                    .diagnostics
                    .iter()
                    .any(|d| d.admission_denial == Some(ManagedAdmissionDenial::InFlight)));
                // Preserve refusal policy: denying C cancels B, rather than
                // weakening revocation to make the adversarial case succeed.
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(3), b)
                        .await
                        .unwrap()
                        .unwrap(),
                    StatusCode::BAD_GATEWAY
                );
                assert_eq!(pending(), 1);
                server.abort();
            }
            HandoffCase::SuccessorAbandonment => {
                assert!(b_still_active && !quiescent);
                b.abort();
                assert!(b.await.unwrap_err().is_cancelled());
                let abandoned = relay.evidence(&id).unwrap();
                assert!(abandoned.revoked && abandoned.uncertain && !abandoned.accounting_complete);
                assert_eq!(
                    abandoned.interruption,
                    Some(ManagedProviderDiagnosticKind::AbandonedForward)
                );
                assert_eq!((abandoned.input_tokens, abandoned.output_tokens), (100, 10));
                assert_eq!(pending(), 1);
                assert!(relay.provider_quiescent(&id));
                assert_eq!(
                    tokio::time::timeout(
                        Duration::from_secs(3),
                        send(&relay, &secret, &request(2))
                    )
                    .await
                    .unwrap()
                    .status(),
                    StatusCode::UNAUTHORIZED
                );
                assert!(
                    forward(&relay.state, &id, &serde_json::to_vec(&request(3)).unwrap())
                        .await
                        .is_err()
                );
                let after = relay.evidence(&id).unwrap();
                assert_eq!(
                    (
                        after.requests_reserved,
                        after.wire_attempts,
                        calls.load(Ordering::SeqCst)
                    ),
                    (2, 2, 2)
                );
                eprintln!(
                    "completed-forward-successor-abandoned {}",
                    json!({"revoked":after.revoked,"uncertain":after.uncertain,"interruption":after.interruption,"providerQuiescent":relay.provider_quiescent(&id),"canonicalPending":pending(),"reserved":after.requests_reserved,"admissions":after.wire_attempts,"fixtureCalls":calls.load(Ordering::SeqCst),"inputTokens":after.input_tokens,"outputTokens":after.output_tokens,"accountingComplete":after.accounting_complete})
                );
                server.abort();
            }
            HandoffCase::Ownership | HandoffCase::Quiescence => {
                if case == HandoffCase::Quiescence {
                    assert!(
                        !quiescent,
                        "completed A cleanup cannot publish quiescence while B is outstanding"
                    );
                }
                assert!(
                    b_still_active,
                    "completed A cleanup cannot clear B's active marker"
                );
                assert!(!quiescent);
                release_b.notify_one();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(3), b)
                        .await
                        .unwrap()
                        .unwrap(),
                    StatusCode::OK
                );
                let complete = relay.evidence(&id).unwrap();
                assert_eq!(
                    (
                        complete.requests_reserved,
                        complete.wire_attempts,
                        complete.responses_completed,
                        calls.load(Ordering::SeqCst)
                    ),
                    (2, 2, 2, 2)
                );
                assert_eq!((complete.input_tokens, complete.output_tokens), (200, 20));
                assert!(complete.accounting_complete && !complete.uncertain && !complete.revoked);
                assert_eq!(pending(), 0);
                assert!(relay.provider_quiescent(&id));
                eprintln!(
                    "completed-forward-normal {}",
                    json!({"revoked":complete.revoked,"uncertain":complete.uncertain,"providerQuiescent":relay.provider_quiescent(&id),"canonicalPending":pending(),"reserved":complete.requests_reserved,"admissions":complete.wire_attempts,"completed":complete.responses_completed,"fixtureCalls":calls.load(Ordering::SeqCst),"inputTokens":complete.input_tokens,"outputTokens":complete.output_tokens,"accountingComplete":complete.accounting_complete})
                );
                server.abort();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_forward_drop_cannot_clear_successor_inflight() {
        assert_completed_forward_handoff(HandoffCase::Ownership).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successor_inflight_prevents_a_third_forward_after_predecessor_drop() {
        assert_completed_forward_handoff(HandoffCase::ThirdRequest).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_guard_drop_does_not_publish_false_quiescence() {
        assert_completed_forward_handoff(HandoffCase::Quiescence).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_predecessor_preserves_successor_abandonment_revocation() {
        assert_completed_forward_handoff(HandoffCase::SuccessorAbandonment).await;
    }

    #[allow(clippy::await_holding_lock)]
    async fn assert_abandoned_forward(draining: bool) {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let calls = Arc::new(AtomicU32::new(0));
        let observed = Arc::new(tokio::sync::Notify::new());
        let count = calls.clone();
        let barrier = observed.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let count = count.clone();
                let barrier = barrier.clone();
                async move {
                    if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        if draining {
                            use futures::StreamExt;
                            let partial = futures::stream::once(async {
                                Ok::<_, std::io::Error>(Bytes::from_static(
                                    b"data: {\"choices\":[]}",
                                ))
                            });
                            let held = futures::stream::once(async move {
                                barrier.notify_one();
                                std::future::pending::<Result<Bytes, std::io::Error>>().await
                            });
                            return (
                                [("content-type", "text/event-stream")],
                                axum::body::Body::from_stream(partial.chain(held)),
                            )
                                .into_response();
                        }
                        barrier.notify_one();
                        std::future::pending::<()>().await;
                    }
                    (
                        [("content-type", "text/event-stream")],
                        stream("settled turn"),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let target = crate::host_helpers::ResolvedModelTarget {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            wire_model: "grok-build-0.1".into(),
            dialect: crate::gateway_config::ProviderDialect::XaiChatCompletions,
            capabilities: crate::gateway_config::ModelCapabilities {
                tools: true,
                stream: true,
                ..Default::default()
            },
            deadline_class: crate::gateway_config::ProviderDeadlineClass::Standard,
        };
        let relay =
            ManagedProviderRelay::with_target(credentials(), target, root.path().join("leases"))
                .await
                .unwrap();
        let (id, secret) = issue(&relay, "abandoned-forward");
        let state = relay.state.clone();
        let forwarding_id = id.clone();
        let task = tokio::spawn(async move {
            forward(
                &state,
                &forwarding_id,
                &serde_json::to_vec(&request(0)).unwrap(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), observed.notified())
            .await
            .unwrap();
        if draining {
            tokio::time::timeout(Duration::from_secs(3), async {
                while relay.evidence(&id).unwrap().http_responses_observed == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let abandoned = relay.evidence(&id).unwrap();
        assert_eq!(abandoned.wire_attempts, 1);
        assert!(abandoned.uncertain);
        assert_eq!(abandoned.http_responses_observed, u32::from(draining));
        assert!(!abandoned.usage_observed && !abandoned.accounting_complete);
        assert_eq!((abandoned.input_tokens, abandoned.output_tokens), (0, 0));
        let custody =
            fs::read_to_string(root.path().join("host/authority/provider-send-v1.key")).unwrap();
        let operator =
            crate::provider_transport::authenticate_provider_reconciliation(&custody).unwrap();
        assert_eq!(
            crate::provider_transport::provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len(),
            1
        );
        let changed_status = send(&relay, &secret, &request(1)).await.status();
        let after = relay.evidence(&id).unwrap();
        server.abort();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "abandoned={abandoned:?}, after={after:?}, changed_status={changed_status}"
        );
        assert_eq!(changed_status, StatusCode::UNAUTHORIZED);
        assert!(abandoned.revoked);
        assert_eq!(
            abandoned.interruption,
            Some(ManagedProviderDiagnosticKind::AbandonedForward)
        );
        assert_eq!(after.requests_reserved, 1);
        assert!(
            forward(&relay.state, &id, &serde_json::to_vec(&request(2)).unwrap())
                .await
                .is_err()
        );
        assert_eq!(relay.evidence(&id).unwrap().requests_reserved, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropped_forward_after_upstream_admission_revokes_capability() {
        assert_abandoned_forward(false).await;
        assert_abandoned_forward(true).await;
    }

    #[tokio::test]
    async fn changed_request_after_abandoned_forward_causes_zero_additional_sends() {
        assert_abandoned_forward(false).await;
        assert_abandoned_forward(true).await;
    }

    #[tokio::test]
    async fn unknown_usage_cannot_be_reused_as_free_budget() {
        assert_abandoned_forward(false).await;
        assert_abandoned_forward(true).await;
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn successful_settled_turn_preserves_bounded_multiturn_execution() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let (relay, calls, server) = relay(root.path().join("leases"), StatusCode::OK).await;
        let id = relay
            .lease_id_for_request("assignment", "multiturn")
            .unwrap();
        relay
            .bind_lease_bounds(&id, 2, LEASE_LIFETIME_MS, MAX_TOTAL_TOKENS)
            .unwrap();
        relay.resolve(&id).unwrap();
        let secret = relay.state.leases.lock().unwrap()[&id].secret.clone();
        for index in 0..2 {
            assert_eq!(
                send(&relay, &secret, &request(index)).await.status(),
                StatusCode::OK
            );
        }
        let evidence = relay.evidence(&id).unwrap();
        assert_eq!(
            (
                evidence.wire_attempts,
                evidence.http_responses_observed,
                evidence.responses_completed
            ),
            (2, 2, 2)
        );
        assert_eq!((evidence.input_tokens, evidence.output_tokens), (200, 20));
        assert!(evidence.accounting_complete && !evidence.uncertain && !evidence.revoked);
        assert_eq!(evidence.remote_effect_uncertain, Some(false));
        assert!(evidence.interruption.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            send(&relay, &secret, &request(2)).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn production_relay_bounds_physical_sends_and_revokes_loaded_capability() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let (relay, calls, server) = relay(root.path().join("leases"), StatusCode::OK).await;
        let (id, secret) = issue(&relay, "bounded");
        assert!(relay.resolve(&id).is_err());
        for index in 0..MAX_REQUESTS {
            assert_eq!(
                send(&relay, &secret, &request(index)).await.status(),
                StatusCode::OK
            );
        }
        assert_eq!(
            send(&relay, &secret, &request(MAX_REQUESTS)).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            send(&relay, &secret, &request(99)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(calls.load(Ordering::SeqCst), MAX_REQUESTS);
        let evidence = relay.evidence(&id).unwrap();
        assert_eq!(evidence.wire_attempts, MAX_REQUESTS);
        assert_eq!(evidence.authority_attempts.len(), MAX_REQUESTS as usize);
        assert_eq!(evidence.output_tokens, u64::from(MAX_REQUESTS) * 10);
        assert!(evidence.revoked);
        assert!(!serde_json::to_string(&evidence).unwrap().contains(&secret));
        let (id, secret) = issue(&relay, "revoked");
        relay.revoke(&id).unwrap();
        relay.revoke(&id).unwrap();
        assert_eq!(
            send(&relay, &secret, &request(0)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(calls.load(Ordering::SeqCst), MAX_REQUESTS);
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn tighter_assignment_budget_cannot_be_amplified_by_relay() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let (relay, calls, server) = relay(root.path().join("leases"), StatusCode::OK).await;
        let id = relay
            .lease_id_for_request("assignment", "one-request")
            .unwrap();
        relay.bind_lease_bounds(&id, 1, 5000, 2000).unwrap();
        assert!(relay.bind_lease_bounds(&id, 2, 5000, 2000).is_err());
        relay.resolve(&id).unwrap();
        assert!(relay.bind_lease_bounds(&id, 1, 5000, 2000).is_err());
        let secret = relay.state.leases.lock().unwrap()[&id].secret.clone();
        assert_eq!(
            send(&relay, &secret, &request(0)).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&relay, &secret, &request(1)).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let id = relay
            .lease_id_for_request("assignment", "tiny-token-budget")
            .unwrap();
        relay.bind_lease_bounds(&id, 1, 5000, 1000).unwrap();
        relay.resolve(&id).unwrap();
        let secret = relay.state.leases.lock().unwrap()[&id].secret.clone();
        assert_eq!(
            send(&relay, &secret, &request(0)).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(relay.evidence(&id).unwrap().wire_attempts, 0);
        assert!(relay.evidence(&id).unwrap().revoked);
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn production_failure_duplicate_and_unsupported_model_cannot_retry() {
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let (bad, calls, server) =
            relay(root.path().join("bad"), StatusCode::SERVICE_UNAVAILABLE).await;
        let (id, secret) = issue(&bad, "failed");
        assert_eq!(
            send(&bad, &secret, &request(0)).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            send(&bad, &secret, &request(1)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(bad.evidence(&id).unwrap().uncertain);
        server.abort();
        let (good, calls, server) = relay(root.path().join("good"), StatusCode::OK).await;
        let (_, secret) = issue(&good, "duplicate");
        assert_eq!(
            send(&good, &secret, &request(0)).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&good, &secret, &request(0)).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            send(&good, &secret, &request(1)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let (_, secret) = issue(&good, "wrong-model");
        let mut wrong = request(0);
        wrong["model"] = json!("other-model");
        assert_eq!(
            send(&good, &secret, &wrong).await.status(),
            StatusCode::BAD_GATEWAY
        );
        let (_, secret) = issue(&good, "wrong-tool");
        let mut wrong = request(0);
        wrong["tools"][0]["function"]["name"] = json!("bash");
        assert_eq!(
            send(&good, &secret, &wrong).await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn production_capability_expiry_cancels_an_inflight_send() {
        let root = tempfile::tempdir().unwrap();
        let (relay, calls, server) = relay(root.path().join("leases"), StatusCode::OK).await;
        let id = relay.lease_id_for_request("assignment", "expired").unwrap();
        relay
            .bind_lease_bounds(&id, 1, 25, MAX_TOTAL_TOKENS)
            .unwrap();
        relay.resolve(&id).unwrap();
        let secret = relay.state.leases.lock().unwrap()[&id].secret.clone();
        relay
            .state
            .leases
            .lock()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .in_flight = true;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(26)).await;
        tokio::task::yield_now().await;
        let evidence = relay.evidence(&id).unwrap();
        assert!(evidence.revoked && evidence.uncertain);
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {secret}").parse().unwrap());
        assert!(authorized_lease(&relay.state, &headers).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[test]
    fn legacy_evidence_retains_unknown_remote_outcome_and_diagnostics_stay_bounded() {
        let mut value = serde_json::to_value(ManagedProviderEvidence {
            uncertain: true,
            wire_attempts: 1,
            ..Default::default()
        })
        .unwrap();
        for key in [
            "remoteEffectUncertain",
            "interruption",
            "diagnostics",
            "httpResponsesObserved",
            "usageObservation",
            "diagnosticsTruncated",
        ] {
            value.as_object_mut().unwrap().remove(key);
        }
        let mut evidence: ManagedProviderEvidence = serde_json::from_value(value).unwrap();
        assert!(evidence.uncertain);
        assert_eq!(evidence.remote_effect_uncertain, None);
        assert!(evidence.usage_observation.is_none());
        for _ in 0..100 {
            evidence.record_diagnostic(ManagedProviderDiagnosticKind::RecoveryUncertain);
        }
        assert_eq!(evidence.diagnostics.len(), 32);
        assert!(evidence.diagnostics_truncated);
        assert_eq!(
            evidence.interruption,
            Some(ManagedProviderDiagnosticKind::RecoveryUncertain)
        );
    }

    #[test]
    fn diagnostic_identifiers_never_retain_known_secrets_or_arbitrary_content() {
        let body = br#"{"error":{"type":"authentication_error","code":"invalid_api_key","message":"SECRET_RESPONSE_CANARY"}}"#;
        assert_eq!(
            safe_error_codes(body, "child-secret", "invalid_api_key").1,
            None
        );
        assert_eq!(
            safe_error_codes(b"SECRET_RESPONSE_CANARY", "child-secret", "upstream-secret"),
            (None, None)
        );
        let mut headers = HeaderMap::new();
        for unsafe_id in [
            "https://host.invalid/?secret=CANARY",
            "COOKIE_CANARY",
            "12345678-1234-4234-8234-123456789abc",
        ] {
            headers.insert("x-request-id", unsafe_id.parse().unwrap());
            assert_eq!(
                safe_request_id(
                    &headers,
                    "child-secret",
                    "12345678-1234-4234-8234-123456789abc"
                ),
                None
            );
        }
    }

    #[test]
    fn incomplete_overbudget_or_secret_stream_is_never_delivered() {
        let good = stream("done");
        let summary =
            validate_completion(good.as_bytes(), "child-secret", "parent-secret").unwrap();
        assert_eq!(
            (summary.0, summary.1),
            (
                0,
                ManagedUsage {
                    input_tokens: 100,
                    output_tokens: 10,
                    total_tokens: 110,
                    cache_read_input_tokens: 0,
                    reasoning_tokens: 0,
                    cost_in_usd_ticks: None,
                }
            )
        );
        assert_eq!(summary.2.receipt_ordinal, 1);
        for malformed in [
            good.replace("[DONE]", ""),
            good.replace("\"stop\"", "\"length\""),
            good.replace("\"completion_tokens\":10", "\"completion_tokens\":99999"),
            good.replace("\"usage\":", "\"missing_usage\":"),
            good.replace("\"total_tokens\":110", "\"total_tokens\":111"),
            good.replace("data: [DONE]", "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"total_tokens\":110}}\n\ndata: [DONE]"),
            stream("child-secret"),
            stream("parent-secret"),
            format!("{good}data: {{}}\n\n"),
        ] {
            assert!(
                validate_completion(malformed.as_bytes(), "child-secret", "parent-secret").is_err()
            );
        }
    }

    #[test]
    fn usage_rejection_subreasons_identify_only_allowlisted_fields() {
        use ManagedUsageField as Field;
        use ManagedUsageRejectionKind as Kind;

        let base = json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110});
        let cases = [
            (
                json!({"completion_tokens":10,"total_tokens":110}),
                Kind::MissingField,
                Some(Field::PromptTokens),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":null,"total_tokens":110}),
                Kind::MalformedField,
                Some(Field::CompletionTokens),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"SECRET_RESPONSE_CANARY":1}),
                Kind::UnsupportedField,
                None,
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":{"SECRET_RESPONSE_CANARY":1}}),
                Kind::UnsupportedDetailsField,
                Some(Field::PromptTokensDetails),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":null}),
                Kind::MalformedDetails,
                Some(Field::PromptTokensDetails),
            ),
            (
                json!({"prompt_tokens":65537,"completion_tokens":10,"total_tokens":65547}),
                Kind::TokenBoundExceeded,
                Some(Field::PromptTokens),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":111}),
                Kind::ConflictingTotal,
                Some(Field::TotalTokens),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":{"cached_tokens":101}}),
                Kind::ConflictingSubset,
                Some(Field::CachedTokens),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"completion_tokens_details":{"reasoning_tokens":11}}),
                Kind::ConflictingSubset,
                Some(Field::ReasoningTokens),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"num_sources_used":1}),
                Kind::UnsupportedSourceCount,
                Some(Field::NumSourcesUsed),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"completion_tokens_details":{"audio_tokens":1}}),
                Kind::UnsupportedNonzeroDetail,
                Some(Field::AudioTokens),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost_in_usd_ticks":null}),
                Kind::CostNull,
                Some(Field::CostInUsdTicks),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost_in_usd_ticks":0}),
                Kind::CostZero,
                Some(Field::CostInUsdTicks),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost_in_usd_ticks":-1}),
                Kind::CostNegative,
                Some(Field::CostInUsdTicks),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost_in_usd_ticks":"SECRET_RESPONSE_CANARY"}),
                Kind::CostTypeUnsupported,
                Some(Field::CostInUsdTicks),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost_in_usd_ticks":1.5}),
                Kind::CostTypeUnsupported,
                Some(Field::CostInUsdTicks),
            ),
            (
                json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"cost_in_usd_ticks":u64::MAX}),
                Kind::CostOutOfRange,
                Some(Field::CostInUsdTicks),
            ),
        ];
        for (value, kind, field) in cases {
            let failure = parse_managed_usage(&value).unwrap_err();
            assert_eq!(
                (failure.rejection.kind, failure.rejection.field),
                (kind, field)
            );
            assert!(!serde_json::to_string(&failure.rejection)
                .unwrap()
                .contains("SECRET_RESPONSE_CANARY"));
            if kind == Kind::TokenBoundExceeded {
                assert_eq!(failure.rejection.observed, None);
                assert_eq!(failure.rejection.expected, Some(65536));
            }
        }
        assert_eq!(parse_managed_usage(&base).unwrap().cost_in_usd_ticks, None);
        let mut priced = base;
        priced["cost_in_usd_ticks"] = json!(777);
        assert_eq!(
            parse_managed_usage(&priced).unwrap().cost_in_usd_ticks,
            Some(777)
        );
        priced["cost_in_usd_ticks"] = Value::Null;
        assert_eq!(
            parse_managed_usage(&priced)
                .unwrap_err()
                .rejection
                .value_state,
            Some(ManagedUsageValueState::Null)
        );
    }

    #[test]
    fn stream_receipt_and_aggregation_failures_have_typed_durable_subreasons() {
        use ManagedUsageField as Field;
        use ManagedUsageRejectionKind as Kind;

        let good = stream("done");
        let missing = good.replace("\"usage\":", "\"omitted_usage\":");
        let failure = validate_completion(missing.as_bytes(), "child", "upstream").unwrap_err();
        assert_eq!(failure.kind, ManagedProviderDiagnosticKind::UsageMissing);
        assert_eq!(failure.usage_rejection.unwrap().kind, Kind::MissingReceipt);
        let repeated = good.replace(
            "data: [DONE]",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"total_tokens\":110}}\n\ndata: [DONE]",
        );
        let failure = validate_completion(repeated.as_bytes(), "child", "upstream").unwrap_err();
        assert_eq!(
            failure.kind,
            ManagedProviderDiagnosticKind::UsageInconsistent
        );
        assert_eq!(failure.usage_rejection.unwrap().kind, Kind::RepeatedReceipt);

        let usage = parse_managed_usage(
            &json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}),
        )
        .unwrap();
        let prior_unknown = ManagedProviderEvidence {
            usage_observed: true,
            total_tokens: None,
            ..Default::default()
        };
        assert_eq!(
            prior_unknown
                .checked_accounting_after(usage)
                .unwrap_err()
                .kind,
            Kind::AggregationPriorUnknown
        );
        let prior_priced = ManagedProviderEvidence {
            usage_observed: true,
            total_tokens: Some(110),
            cost_in_usd_ticks: Some(777),
            ..Default::default()
        };
        assert_eq!(
            prior_priced
                .checked_accounting_after(usage)
                .unwrap_err()
                .kind,
            Kind::AggregationMixedCostPresence
        );
        let prior_overflow = ManagedProviderEvidence {
            usage_observed: true,
            total_tokens: Some(u64::MAX),
            ..Default::default()
        };
        let rejection = prior_overflow.checked_accounting_after(usage).unwrap_err();
        assert_eq!(
            (rejection.kind, rejection.field),
            (Kind::AggregationOverflow, Some(Field::TotalTokens))
        );

        let mut diagnostic = ManagedProviderDiagnostic {
            kind: ManagedProviderDiagnosticKind::UsageInconsistent,
            admission: 1,
            http_status: None,
            provider_request_id: None,
            provider_error_type: None,
            provider_error_code: None,
            admission_denial: None,
            request_bytes: None,
            usage_rejection: Some(rejection),
        };
        let roundtrip: ManagedProviderDiagnostic =
            serde_json::from_value(serde_json::to_value(&diagnostic).unwrap()).unwrap();
        assert_eq!(roundtrip, diagnostic);
        diagnostic.usage_rejection = None;
        let legacy = serde_json::to_value(&diagnostic).unwrap();
        assert!(legacy.get("usageRejection").is_none());
        let restored: ManagedProviderDiagnostic = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.usage_rejection, None);
    }

    /// Runs the installed binary and the actual adapter/confinement against
    /// a local cost-isolating upstream. This is not live qualification.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires the installed Grok 1.0.41 CLI and native macOS sandbox"]
    #[allow(clippy::await_holding_lock)]
    async fn installed_cli_obeys_production_protocol_and_failure_has_no_retry() {
        use crate::grok_build::{launch_grok_build, GrokBuildHostLaunchConfig};
        use grokptah_agent_sdk::{
            GrokBuildGitIdentity, GrokBuildLaunchRequest, GrokBuildMutationMode, GrokBuildRunState,
        };
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        let cli = PathBuf::from(
            std::env::var("GROKPTAH_REAL_GROK_CLI").expect("installed CLI path required"),
        );
        let _serial = crate::home_override_serial();
        let root = tempfile::tempdir().unwrap();
        crate::set_grokptah_home_override(Some(root.path().join("host")));
        let _home = Home;
        let source = root.path().join("source");
        let isolate = root.path().join("workers");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&isolate).unwrap();
        fs::set_permissions(&isolate, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(source.join("source.txt"), "unchanged source").unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("/usr/bin/git")
                .current_dir(&source)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-b", "qualification"]);
        git(&["config", "user.name", "Protocol probe"]);
        git(&["config", "user.email", "probe@grokptah.invalid"]);
        git(&["add", "source.txt"]);
        git(&["commit", "-m", "Protocol probe source"]);
        let head = git(&["rev-parse", "HEAD"]);
        let identity = GrokBuildGitIdentity {
            repository_id: "protocol-probe".into(),
            git_ref: "refs/heads/qualification".into(),
            base_sha: head.clone(),
            head_sha: head,
        };
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let populated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watched = isolate.clone();
        let stop_thread = stop.clone();
        let populated_thread = populated.clone();
        let monitor = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                if fs::read_dir(&watched).unwrap().next().is_some() {
                    populated_thread.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let ready = crate::grok_build::managed_offline_readiness(&cli, &isolate);
        stop.store(true, Ordering::SeqCst);
        monitor.join().unwrap();
        assert_eq!(ready.unwrap(), "1.0.41");
        assert!(
            !populated.load(Ordering::SeqCst),
            "status probe must not populate the empty launch root"
        );
        for (index, status) in [
            StatusCode::OK,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::REQUEST_TIMEOUT,
        ]
        .into_iter()
        .enumerate()
        {
            let (relay, calls, server) =
                relay(root.path().join(format!("leases-{index}")), status).await;
            let request = uuid::Uuid::new_v4().to_string();
            let lease = relay.lease_id_for_request("protocol", &request).unwrap();
            let launch = GrokBuildLaunchRequest {
                request_id: request,
                identity: identity.clone(),
                mutation_mode: GrokBuildMutationMode::IsolatedReview,
                max_prompt_bytes: 1024,
                max_turns: 6,
                max_duration_ms: 30000,
                credential_lease_id: lease.clone(),
            };
            let config = GrokBuildHostLaunchConfig {executable:cli.clone(),git_executable:"/usr/bin/git".into(),cwd:source.clone(),repository_id:identity.repository_id.clone(),base_ref:identity.git_ref.clone(),prompt:"Do not change files. Finish with a short description and GROK_BUILD_VERDICT=not_complete.".into(),allowed_files:vec!["source.txt".into()],execution_approved:true,max_stdout_bytes:65536,max_stderr_bytes:65536,git_timeout:Duration::from_secs(3),isolate_parent:isolate.clone(),defer_source_apply:true,candidate_retention_dir:Some(root.path().join(format!("candidate-{index}")))};
            let cancel = CancellationToken::new();
            if status == StatusCode::REQUEST_TIMEOUT {
                let cancel = cancel.clone();
                let observed = calls.clone();
                tokio::spawn(async move {
                    while observed.load(Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    cancel.cancel();
                });
            }
            let outcome = launch_grok_build(&launch, &config, relay.as_ref(), cancel).await;
            let accounting = relay.evidence(&lease).unwrap();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "{outcome:?} {accounting:?}"
            );
            assert!(accounting.revoked);
            assert_eq!(accounting.wire_attempts, 1);
            if status == StatusCode::OK {
                let outcome = outcome.unwrap();
                assert_eq!(
                    outcome.result().state,
                    GrokBuildRunState::CompleteAdvisory,
                    "{:?}",
                    outcome.result().evidence_refs
                );
                assert!(accounting.accounting_complete && accounting.usage_observed);
            } else {
                assert!(
                    outcome.is_err()
                        || outcome.unwrap().result().state != GrokBuildRunState::CompleteAdvisory
                );
                assert!(accounting.uncertain);
            }
            assert_eq!(
                fs::read_to_string(source.join("source.txt")).unwrap(),
                "unchanged source"
            );
            assert!(git(&["status", "--porcelain"]).is_empty());
            relay.stop_authority();
            server.abort();
        }
    }
}
