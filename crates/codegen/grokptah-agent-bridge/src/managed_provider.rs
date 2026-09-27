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
            .input_tokens
            .saturating_add(lease.evidence.output_tokens)
            .saturating_add(bytes.len() as u64)
            .saturating_add(u64::from(MAX_OUTPUT_TOKENS))
            > lease.max_total_tokens
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
    )
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
        Err(reason) => {
            note_diagnostic(
                state,
                id,
                if reason == "managed stream usage is missing" {
                    ManagedProviderDiagnosticKind::UsageMissing
                } else if reason.contains("usage") {
                    ManagedProviderDiagnosticKind::UsageInconsistent
                } else {
                    ManagedProviderDiagnosticKind::ProtocolFailure
                },
            );
            let _ = response.settle_protocol_error(reason);
            invalidate(state, id);
            return Err(reason);
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
        lease.evidence.tool_calls += summary.0;
        if let Some((input, output)) = summary.1 {
            if lease
                .evidence
                .input_tokens
                .saturating_add(lease.evidence.output_tokens)
                .saturating_add(input)
                .saturating_add(output)
                > lease.max_total_tokens
            {
                drop(leases);
                let _ = response.settle_protocol_error("managed aggregate token budget exceeded");
                invalidate(state, id);
                return Err("managed aggregate token budget exceeded");
            }
            lease.evidence.input_tokens += input;
            lease.evidence.output_tokens += output;
            lease.evidence.usage_observed = true;
        }
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
        };
        if self.diagnostics.len() < 32 {
            self.diagnostics.push(diagnostic);
        } else if let Some(last) = self.diagnostics.last_mut() {
            *last = diagnostic;
        }
    }
}
fn note_diagnostic(state: &RelayState, id: &str, kind: ManagedProviderDiagnosticKind) {
    if let Ok(mut leases) = state.leases.lock() {
        if let Some(lease) = leases.get_mut(id) {
            record_diagnostic(lease, kind);
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

type CompletionSummary = (u32, Option<(u64, u64)>);
fn validate_completion(
    bytes: &[u8],
    child_secret: &str,
    upstream_secret: &str,
) -> Result<CompletionSummary, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "managed response is not UTF-8")?;
    if text.contains(child_secret) || text.contains(upstream_secret) {
        return Err("managed response contains credential material");
    }
    let mut finished = false;
    let mut done = false;
    let mut tools = BTreeMap::<u64, (String, String)>::new();
    let mut usage = None;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if done {
            return Err("managed stream contains data after its end marker");
        }
        if data == "[DONE]" {
            done = true;
            continue;
        }
        let value: Value = serde_json::from_str(data).map_err(|_| "managed stream is malformed")?;
        if value.get("error").is_some() {
            return Err("managed stream contains a provider error");
        }
        if let Some(u) = value.get("usage").filter(|u| !u.is_null()) {
            if usage.is_some() {
                return Err("managed stream repeated its usage receipt");
            }
            let input = u["prompt_tokens"]
                .as_u64()
                .ok_or("managed usage is malformed")?;
            let output = u["completion_tokens"]
                .as_u64()
                .ok_or("managed usage is malformed")?;
            if output > u64::from(MAX_OUTPUT_TOKENS)
                || input > MAX_REQUEST_BYTES as u64
                || u["total_tokens"].as_u64() != Some(input.saturating_add(output))
            {
                return Err("managed usage is inconsistent or exceeds token bounds");
            }
            usage = Some((input, output));
        }
        let choices = value["choices"]
            .as_array()
            .ok_or("managed choices are malformed")?;
        if choices.len() > 1 {
            return Err("managed stream returned multiple choices");
        }
        for choice in choices {
            if finished || choice["index"].as_u64() != Some(0) {
                return Err("managed stream continued after its finish reason");
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                if !matches!(reason, "stop" | "tool_calls") {
                    return Err("managed response did not complete within its bounds");
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
                    return Err("managed tool arguments exceed their bounds");
                }
            }
        }
    }
    if !finished || !done {
        return Err("managed stream did not prove a completed response");
    }
    if usage.is_none() {
        return Err("managed stream usage is missing");
    }
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
            return Err("managed response requested an unsupported tool");
        }
    }
    Ok((tools.len() as u32, usage))
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

    fn stream(text: &str) -> String {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"id":"test", "object":"chat.completion.chunk", "created":0, "model":"grok-build-0.1", "choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":"stop"}]}),
            json!({"id":"test", "object":"chat.completion.chunk", "created":0, "model":"grok-build-0.1", "choices":[], "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}})
        )
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
        ] {
            value.as_object_mut().unwrap().remove(key);
        }
        let mut evidence: ManagedProviderEvidence = serde_json::from_value(value).unwrap();
        assert!(evidence.uncertain);
        assert_eq!(evidence.remote_effect_uncertain, None);
        for _ in 0..100 {
            evidence.record_diagnostic(ManagedProviderDiagnosticKind::RecoveryUncertain);
        }
        assert_eq!(evidence.diagnostics.len(), 32);
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
        assert_eq!(
            validate_completion(good.as_bytes(), "child-secret", "parent-secret").unwrap(),
            (0, Some((100, 10)))
        );
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
