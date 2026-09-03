//! The only credential-bearing provider wire-emission boundary.
//!
//! Callers construct a complete `reqwest::Request`, but only this module may
//! hand it to the HTTP client. Immediately before that handoff it obtains a
//! one-use [`PhysicalSendPermit`] from the canonical host-authority root. The
//! permit is then consumed by exactly one terminal settlement.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use bytes::Bytes;
use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use xai_host_authority::{
    ActorClass, AttemptId, AuthContext, ContentDigest, EffectClass, FailedReason,
    HostAdminAuthority, HostAdminCredential, HostAuthority, HostCredential, PhysicalSendPermit,
    ReconciliationDisposition, ReconciliationEvidence, RequestIdentity, SendOutcome, UncertainReason,
};

const AUTHORITY_DIR: &str = "authority/provider-send-v1";
const CUSTODY_FILE: &str = "authority/provider-send-v1.key";
const SERVICE_CREDENTIAL_ID: &str = "provider-transport";
const CAPABILITY_TTL_MS: u64 = 60_000;
const LEASE_TTL_MS: u64 = 30_000;
const RECONCILE_GRANT_TTL_MS: u64 = 60_000;
const IDEMPOTENCY_KEY_HEADER: HeaderName = HeaderName::from_static("idempotency-key");

static AUTHORITIES: OnceLock<Mutex<HashMap<PathBuf, Arc<ProviderAuthority>>>> = OnceLock::new();

struct ProviderAuthority {
    root: PathBuf,
    authority: HostAuthority,
    admin: HostAdminAuthority,
    service_bearer: String,
}

/// Non-forgeable, non-cloneable proof that the caller authenticated with the
/// provider-authority custody secret. The secret itself is never retained.
#[must_use = "provider reconciliation requires an authenticated operator capability"]
pub struct ProviderReconciliationAuthority {
    root: PathBuf,
}

impl std::fmt::Debug for ProviderReconciliationAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderReconciliationAuthority([redacted])")
    }
}

/// Secret-bearing material used only to bind the permit to the exact wire
/// request. It is digested synchronously and is never retained by authority.
pub(crate) struct ProviderRequestScope<'a> {
    pub credential_secret: &'a [u8],
    pub dialect: &'a str,
    pub model: &'a str,
    /// Secret-free logical surface (for example `agent-step` or
    /// `provider-qualification`). This participates in the resource binding.
    pub target_scope: &'a str,
}

#[derive(Debug)]
pub(crate) struct ProviderTransportError {
    message: String,
    outcome: Option<SendOutcome>,
}

/// HTTP statuses whose response does not prove that a provider-side effect
/// was absent. Callers must retain these attempts as ambiguous rather than
/// interpreting the status as permission to spend a fresh attempt.
pub(crate) fn is_retry_oriented_http_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.as_u16() == 429
        || status.is_server_error()
}

impl ProviderTransportError {
    fn before_dispatch(error: impl std::fmt::Display) -> Self {
        Self {
            message: format!("provider send authority refused before dispatch: {error}"),
            outcome: None,
        }
    }

    fn settled(message: impl Into<String>, outcome: SendOutcome) -> Self {
        Self {
            message: message.into(),
            outcome: Some(outcome),
        }
    }

    /// Only a proven connect-time refusal can be retried automatically. An
    /// absent outcome is an authority denial and also cannot be retried by a
    /// lower layer: policy must explicitly grant a new attempt.
    pub(crate) fn is_safe_to_resend(&self) -> bool {
        self.outcome
            .as_ref()
            .is_some_and(SendOutcome::is_safe_to_resend)
    }

    pub(crate) fn is_uncertain(&self) -> bool {
        self.outcome
            .as_ref()
            .is_some_and(|outcome| matches!(outcome, SendOutcome::Uncertain { .. }))
    }

    pub(crate) fn attempt_handle(&self) -> Option<String> {
        self.outcome.as_ref().map(|outcome| match outcome {
            SendOutcome::Settled { attempt }
            | SendOutcome::Failed { attempt, .. }
            | SendOutcome::Uncertain { attempt, .. } => attempt.public_handle(),
        })
    }
}

impl std::fmt::Display for ProviderTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderTransportError {}

enum WireResult {
    Response(reqwest::Response),
    RefusedBeforeWrite(String),
    Ambiguous(String),
}

/// A response whose physical-send permit remains live until the caller has
/// consumed and validated the complete provider protocol. Dropping it, losing
/// the body, or abandoning a partial stream settles the attempt Uncertain.
pub(crate) struct ProviderResponse {
    response: reqwest::Response,
    runtime: Arc<ProviderAuthority>,
    permit: Option<PhysicalSendPermit>,
}

impl std::fmt::Debug for ProviderResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderResponse")
            .field("status", &self.response.status())
            .field(
                "attempt",
                &self.permit.as_ref().map(PhysicalSendPermit::attempt),
            )
            .finish_non_exhaustive()
    }
}

impl ProviderResponse {
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub(crate) fn headers(&self) -> &reqwest::header::HeaderMap {
        self.response.headers()
    }

    pub(crate) fn content_length(&self) -> Option<u64> {
        self.response.content_length()
    }

    pub(crate) fn attempt_handle(&self) -> Option<String> {
        self.permit
            .as_ref()
            .map(|permit| permit.attempt().public_handle())
    }

    pub(crate) async fn next_chunk(
        &mut self,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<Bytes>, ProviderTransportError> {
        let result = if let Some(cancel) = cancel {
            tokio::select! {
                result = self.response.chunk() => result,
                _ = cancel.cancelled() => {
                    return Err(self.settle_uncertain_error(
                        "provider response cancelled after response headers",
                        UncertainReason::CancelledAfterPossibleWrite,
                    ));
                }
            }
        } else {
            self.response.chunk().await
        };
        match result {
            Ok(chunk) => Ok(chunk),
            Err(_) => Err(self.settle_uncertain_error(
                "provider response body failed after response headers",
                UncertainReason::ResponseBodyAfterPossibleEffect,
            )),
        }
    }

    pub(crate) fn settle_success(mut self) -> Result<(), ProviderTransportError> {
        let Some(permit) = self.permit.take() else {
            return Err(ProviderTransportError::before_dispatch(
                "provider response no longer owns a send permit",
            ));
        };
        let outcome = self.runtime.authority.settle_settled(permit);
        if matches!(outcome, SendOutcome::Settled { .. }) {
            Ok(())
        } else {
            Err(ProviderTransportError::settled(
                "provider response was validated but settlement is not durable",
                outcome,
            ))
        }
    }

    pub(crate) fn settle_protocol_error(
        mut self,
        message: impl Into<String>,
    ) -> ProviderTransportError {
        let Some(permit) = self.permit.take() else {
            return ProviderTransportError::before_dispatch(
                "provider response no longer owns a send permit",
            );
        };
        let outcome = self
            .runtime
            .authority
            .settle_uncertain(permit, UncertainReason::ProtocolAfterPossibleEffect);
        ProviderTransportError::settled(message, outcome)
    }

    /// Settle a complete HTTP failure according to the status that was
    /// physically observed. Retry-oriented statuses remain ambiguous because
    /// they do not prove that the provider omitted the requested effect; other
    /// complete HTTP refusals are definitive and may settle normally.
    pub(crate) fn settle_http_failure(
        mut self,
        message: impl Into<String>,
    ) -> ProviderTransportError {
        let Some(permit) = self.permit.take() else {
            return ProviderTransportError::before_dispatch(
                "provider response no longer owns a send permit",
            );
        };
        let outcome = if is_retry_oriented_http_status(self.response.status()) {
            self.runtime
                .authority
                .settle_uncertain(permit, UncertainReason::ProtocolAfterPossibleEffect)
        } else {
            self.runtime.authority.settle_settled(permit)
        };
        ProviderTransportError::settled(message, outcome)
    }

    fn settle_uncertain_error(
        &mut self,
        message: impl Into<String>,
        reason: UncertainReason,
    ) -> ProviderTransportError {
        let Some(permit) = self.permit.take() else {
            return ProviderTransportError::before_dispatch(
                "provider response no longer owns a send permit",
            );
        };
        let outcome = self.runtime.authority.settle_uncertain(permit, reason);
        ProviderTransportError::settled(message, outcome)
    }
}

impl Drop for ProviderResponse {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            let _ = self
                .runtime
                .authority
                .settle_uncertain(permit, UncertainReason::ResponseBodyAfterPossibleEffect);
        }
    }
}

#[async_trait]
trait WireDispatch: Send + Sync {
    async fn dispatch(&self, client: &reqwest::Client, request: reqwest::Request) -> WireResult;
}

struct ReqwestWireDispatch;

#[async_trait]
impl WireDispatch for ReqwestWireDispatch {
    async fn dispatch(&self, client: &reqwest::Client, request: reqwest::Request) -> WireResult {
        match client.execute(request).await {
            Ok(response) => WireResult::Response(response),
            Err(error) if error.is_connect() => {
                WireResult::RefusedBeforeWrite("connection refused before request write".into())
            }
            Err(error) => WireResult::Ambiguous(if error.is_timeout() {
                "provider request timed out after dispatch began".into()
            } else {
                "provider transport failed after dispatch began".into()
            }),
        }
    }
}

/// Execute one credential-bearing request through the canonical physical-send
/// lattice. The returned response retains the one-use send permit until the
/// caller consumes and validates the complete provider protocol, then settles
/// it explicitly. Dropping or failing the body marks the attempt uncertain.
pub(crate) async fn send_provider_request(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    scope: ProviderRequestScope<'_>,
    cancel: Option<&CancellationToken>,
) -> Result<ProviderResponse, ProviderTransportError> {
    send_provider_request_with(client, request, scope, cancel, &ReqwestWireDispatch).await
}

async fn send_provider_request_with(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    scope: ProviderRequestScope<'_>,
    cancel: Option<&CancellationToken>,
    dispatch: &dyn WireDispatch,
) -> Result<ProviderResponse, ProviderTransportError> {
    let mut request = request
        .build()
        .map_err(ProviderTransportError::before_dispatch)?;
    let body =
        validate_wire_request(&request, &scope).map_err(ProviderTransportError::before_dispatch)?;
    let identity = RequestIdentity::new(
        request.url().as_str(),
        request.method().as_str(),
        scope.dialect,
        scope.credential_secret,
        scope.model,
        body,
    );
    let runtime = authority_runtime().map_err(ProviderTransportError::before_dispatch)?;
    let (auth, permit) = runtime
        .permit(&identity, scope.target_scope)
        .map_err(ProviderTransportError::before_dispatch)?;
    if dialect_supports_wire_idempotency(permit.dialect()) {
        if let Err(error) = bind_idempotency_key(&mut request, &identity, &permit) {
            let outcome = runtime
                .authority
                .settle_failed_before_write(permit, FailedReason::DeniedBeforeDispatch);
            return Err(ProviderTransportError::settled(error.to_string(), outcome));
        }
    }
    let permit = runtime
        .authority
        .admit_sending(&auth, permit)
        .map_err(ProviderTransportError::before_dispatch)?;

    let result = if let Some(cancel) = cancel {
        tokio::select! {
            result = dispatch.dispatch(client, request) => result,
            _ = cancel.cancelled() => {
                let outcome = runtime.authority.settle_uncertain(
                    permit,
                    UncertainReason::CancelledAfterPossibleWrite,
                );
                return Err(ProviderTransportError::settled(
                    "provider request cancelled after dispatch began; outcome is uncertain",
                    outcome,
                ));
            }
        }
    } else {
        dispatch.dispatch(client, request).await
    };

    match result {
        WireResult::Response(response) => Ok(ProviderResponse {
            response,
            runtime,
            permit: Some(permit),
        }),
        WireResult::RefusedBeforeWrite(message) => {
            let outcome = runtime
                .authority
                .settle_failed_before_write(permit, FailedReason::ConnectRefusedBeforeWrite);
            Err(ProviderTransportError::settled(message, outcome))
        }
        WireResult::Ambiguous(message) => {
            let outcome = runtime
                .authority
                .settle_uncertain(permit, UncertainReason::TransportAfterPossibleWrite);
            Err(ProviderTransportError::settled(message, outcome))
        }
    }
}

fn validate_wire_request<'a>(
    request: &'a reqwest::Request,
    scope: &ProviderRequestScope<'_>,
) -> anyhow::Result<&'a [u8]> {
    let body = match request.body() {
        None => &[][..],
        Some(body) => body
            .as_bytes()
            .ok_or_else(|| anyhow!("provider request body is not immutable bytes"))?,
    };

    if request.headers().contains_key(&IDEMPOTENCY_KEY_HEADER) {
        return Err(anyhow!(
            "provider caller must not supply or override the host idempotency key"
        ));
    }

    let method = request.method();
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match scope.dialect {
        "openai_model_catalog" => {
            validate_bearer_credential(request, body, scope.credential_secret)?;
            if method != reqwest::Method::GET || !body.is_empty() || scope.model != "model-catalog"
            {
                return Err(anyhow!(
                    "model-catalog wire shape does not match its dialect"
                ));
            }
        }
        "oauth2_refresh" => {
            if method != reqwest::Method::POST
                || !content_type.starts_with("application/x-www-form-urlencoded")
                || body.is_empty()
                || scope.model != "oidc-token-refresh"
            {
                return Err(anyhow!(
                    "OAuth refresh wire shape does not match its dialect"
                ));
            }
            validate_oauth_refresh_credential(request, body, scope.credential_secret)?;
        }
        "xai_chat_completions" | "openai_chat_completions" | "provider_qualification" => {
            validate_bearer_credential(request, body, scope.credential_secret)?;
            if method != reqwest::Method::POST
                || !content_type.starts_with("application/json")
                || body.is_empty()
            {
                return Err(anyhow!(
                    "provider completion wire shape does not match its dialect"
                ));
            }
            let value: serde_json::Value = serde_json::from_slice(body)
                .context("provider request body is not canonical JSON")?;
            if value.get("model").and_then(serde_json::Value::as_str) != Some(scope.model) {
                return Err(anyhow!("provider model scope does not match request body"));
            }
        }
        _ => return Err(anyhow!("unsupported provider wire dialect")),
    }
    Ok(body)
}

fn validate_bearer_credential(
    request: &reqwest::Request,
    body: &[u8],
    credential_secret: &[u8],
) -> anyhow::Result<()> {
    let actual_authorization = request.headers().get(AUTHORIZATION);
    if request.headers().get_all(AUTHORIZATION).iter().count() > 1 {
        return Err(anyhow!("provider request repeats its Authorization header"));
    }
    if credential_secret.is_empty() {
        if actual_authorization.is_some() {
            return Err(anyhow!(
                "provider credential scope does not match request headers"
            ));
        }
        return Ok(());
    }
    let secret =
        std::str::from_utf8(credential_secret).context("provider credential is not valid UTF-8")?;
    let expected = format!("Bearer {secret}");
    let actual = actual_authorization
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow!("provider request is missing its admitted credential"))?;
    if !constant_time_secret_eq(actual.as_bytes(), expected.as_bytes()) {
        return Err(anyhow!(
            "provider credential scope does not match request headers"
        ));
    }
    if !body.is_empty()
        && body
            .windows(credential_secret.len())
            .any(|window| window == credential_secret)
    {
        return Err(anyhow!(
            "provider credential appears in more than one wire location"
        ));
    }
    Ok(())
}

fn validate_oauth_refresh_credential(
    request: &reqwest::Request,
    body: &[u8],
    credential_secret: &[u8],
) -> anyhow::Result<()> {
    if request.headers().contains_key(AUTHORIZATION) {
        return Err(anyhow!(
            "OAuth refresh credential must not also appear in Authorization"
        ));
    }
    if credential_secret.is_empty() {
        return Err(anyhow!("OAuth refresh credential is absent"));
    }
    let expected = std::str::from_utf8(credential_secret)
        .context("OAuth refresh credential is not valid UTF-8")?;
    let mut refresh_tokens = url::form_urlencoded::parse(body)
        .filter_map(|(key, value)| (key == "refresh_token").then_some(value.into_owned()));
    let actual = refresh_tokens
        .next()
        .ok_or_else(|| anyhow!("OAuth refresh request is missing its admitted credential"))?;
    if refresh_tokens.next().is_some() {
        return Err(anyhow!("OAuth refresh request repeats its credential"));
    }
    if !constant_time_secret_eq(actual.as_bytes(), expected.as_bytes()) {
        return Err(anyhow!(
            "OAuth refresh credential scope does not match request body"
        ));
    }
    Ok(())
}

/// Whether the wire dialect explicitly supports a host-bound idempotency key.
pub(crate) fn dialect_supports_wire_idempotency(dialect: &str) -> bool {
    matches!(
        dialect,
        "xai_chat_completions" | "openai_chat_completions" | "provider_qualification"
    )
}

fn bind_idempotency_key(
    request: &mut reqwest::Request,
    identity: &RequestIdentity,
    permit: &PhysicalSendPermit,
) -> anyhow::Result<()> {
    if !dialect_supports_wire_idempotency(permit.dialect()) {
        return Ok(());
    }
    if permit.request_digest() != identity.digest() {
        return Err(anyhow!(
            "provider permit does not match the admitted request identity"
        ));
    }
    if request.headers().contains_key(&IDEMPOTENCY_KEY_HEADER) {
        return Err(anyhow!(
            "provider request already contains an idempotency key"
        ));
    }
    let value = HeaderValue::from_str(permit.idempotency_key())
        .context("host idempotency key is not a valid header value")?;
    request.headers_mut().insert(IDEMPOTENCY_KEY_HEADER, value);
    if request
        .headers()
        .get(&IDEMPOTENCY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        != Some(permit.idempotency_key())
    {
        return Err(anyhow!("provider idempotency binding mismatch"));
    }
    Ok(())
}

impl ProviderAuthority {
    fn permit(
        &self,
        request: &RequestIdentity,
        target_scope: &str,
    ) -> anyhow::Result<(AuthContext, PhysicalSendPermit)> {
        let auth = self.authority.authenticate(&self.service_bearer)?;
        let cwd = std::env::current_dir().context("resolve provider-send workspace")?;
        let workspace = self.authority.issue_workspace(&auth, &cwd)?;
        let resource =
            self.authority
                .obtain_provider_send_surface(&auth, workspace, target_scope)?;
        let capability = self.authority.seal_capability(
            &auth,
            resource,
            ActorClass::VerifiedModel,
            EffectClass::ProviderSend,
            CAPABILITY_TTL_MS,
        )?;
        let lease =
            self.authority
                .mint_lease(&auth, &capability, request.digest(), LEASE_TTL_MS)?;
        let permit = self
            .authority
            .begin_send(&auth, lease, request, target_scope)?;
        Ok((auth, permit))
    }
}

fn authority_runtime() -> anyhow::Result<Arc<ProviderAuthority>> {
    let home = crate::discover::grokptah_home();
    let root = home.join(AUTHORITY_DIR);
    let registry = AUTHORITIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| anyhow!("provider authority registry poisoned"))?;
    if let Some(runtime) = registry.get(&root) {
        return Ok(Arc::clone(runtime));
    }

    fs::create_dir_all(&home).context("create GrokPtah home for provider authority")?;
    fs::create_dir_all(&root).context("create provider authority root")?;
    secure_authority_directory(&home.join("authority"))?;
    secure_authority_directory(&root)?;
    let custody = load_or_create_custody(&home.join(CUSTODY_FILE))?;
    let admin = HostAdminCredential::new(custody.clone())?;
    let (authority, admin_authority) = HostAuthority::open(&root, &admin)?;
    // Crash-left sends must be classified before this incarnation can admit
    // another provider effect. Recovery never resends; it publishes the
    // attempts that require explicit operator reconciliation.
    authority.recover_incomplete(&admin_authority)?;
    let service_bearer = derive_service_bearer(&custody);
    authority.set_credentials(
        &admin_authority,
        &[HostCredential::new(
            SERVICE_CREDENTIAL_ID,
            service_bearer.clone(),
        )?],
    )?;
    let runtime = Arc::new(ProviderAuthority {
        root: root.clone(),
        authority,
        admin: admin_authority,
        service_bearer,
    });
    registry.insert(root, Arc::clone(&runtime));
    Ok(runtime)
}

/// Resolve one previously ambiguous provider attempt from independently
/// established operator/provider truth. No resend occurs here.
pub(crate) fn reconcile_provider_attempt(
    operator: &ProviderReconciliationAuthority,
    attempt: AttemptId,
    took_effect: bool,
) -> anyhow::Result<()> {
    let runtime = authority_runtime()?;
    runtime.require_reconciliation_authority(operator)?;
    let auth = runtime.authority.authenticate(&runtime.service_bearer)?;
    let disposition = if took_effect {
        ReconciliationDisposition::MarkSettled
    } else {
        ReconciliationDisposition::MarkNotSent
    };
    let evidence = operator_reconciliation_evidence(attempt, took_effect);
    let grant = runtime.authority.mint_reconciliation_grant_for_attempt(
        &auth,
        attempt,
        disposition,
        RECONCILE_GRANT_TTL_MS,
    )?;
    runtime
        .authority
        .apply_reconciliation(&auth, grant, evidence)?;
    Ok(())
}

fn operator_reconciliation_evidence(attempt: AttemptId, took_effect: bool) -> ReconciliationEvidence {
    let digest = ContentDigest::of_fields(&[
        ("provider-reconcile-bridge-v1", b""),
        ("attempt", attempt.public_handle().as_bytes()),
        ("took_effect", &[u8::from(took_effect)]),
    ]);
    if took_effect {
        ReconciliationEvidence::provider_receipt(digest)
    } else {
        ReconciliationEvidence::operator_observation(digest)
    }
}

pub(crate) fn provider_attempts_requiring_reconciliation(
    operator: &ProviderReconciliationAuthority,
) -> anyhow::Result<Vec<AttemptId>> {
    let runtime = authority_runtime()?;
    runtime.require_reconciliation_authority(operator)?;
    Ok(runtime.authority.ambiguous_attempts(&runtime.admin)?)
}

pub(crate) fn authenticate_provider_reconciliation(
    custody_secret: &str,
) -> anyhow::Result<ProviderReconciliationAuthority> {
    let runtime = authority_runtime()?;
    let expected = load_or_create_custody(&crate::discover::grokptah_home().join(CUSTODY_FILE))?;
    if !constant_time_secret_eq(custody_secret.as_bytes(), expected.as_bytes()) {
        return Err(anyhow!(
            "provider reconciliation operator is unauthenticated"
        ));
    }
    Ok(ProviderReconciliationAuthority {
        root: runtime.root.clone(),
    })
}

impl ProviderAuthority {
    fn require_reconciliation_authority(
        &self,
        operator: &ProviderReconciliationAuthority,
    ) -> anyhow::Result<()> {
        if operator.root == self.root {
            Ok(())
        } else {
            Err(anyhow!(
                "provider reconciliation authority belongs to another root"
            ))
        }
    }
}

fn constant_time_secret_eq(left: &[u8], right: &[u8]) -> bool {
    let left = Sha256::digest(left);
    let right = Sha256::digest(right);
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn secure_authority_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn derive_service_bearer(custody: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"grokptah-provider-transport-service-v1\0");
    digest.update(custody.as_bytes());
    format!("{:x}", digest.finalize())
}

fn load_or_create_custody(path: &Path) -> anyhow::Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match create_custody(path) {
        Ok(secret) => Ok(secret),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_custody(path),
        Err(error) => Err(error).context("create provider authority custody key"),
    }
}

fn create_custody(path: &Path) -> std::io::Result<String> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let secret = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    file.write_all(secret.as_bytes())?;
    file.sync_all()?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider custody path has no parent directory",
        )
    })?;
    File::open(parent)?.sync_all()?;
    Ok(secret)
}

fn read_custody(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = fs::metadata(path)?.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(anyhow!(
                "provider authority custody key must not be group/world accessible"
            ));
        }
    }
    let mut value = String::new();
    OpenOptions::new()
        .read(true)
        .open(path)?
        .take(4096)
        .read_to_string(&mut value)?;
    let value = value.trim().to_string();
    if value.len() < 32 {
        return Err(anyhow!("provider authority custody key is incomplete"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct HomeOverride;

    impl HomeOverride {
        fn install(path: PathBuf) -> Self {
            crate::discover::set_grokptah_home_override(Some(path));
            Self
        }
    }

    impl Drop for HomeOverride {
        fn drop(&mut self) {
            crate::discover::set_grokptah_home_override(None);
        }
    }

    struct InjectedDispatch {
        calls: AtomicUsize,
        result: Mutex<Option<WireResult>>,
    }

    struct PendingDispatch;

    struct HeaderCaptureDispatch {
        idempotency_key: Mutex<Option<String>>,
    }

    #[async_trait]
    impl WireDispatch for PendingDispatch {
        async fn dispatch(
            &self,
            _client: &reqwest::Client,
            _request: reqwest::Request,
        ) -> WireResult {
            std::future::pending::<WireResult>().await
        }
    }

    #[async_trait]
    impl WireDispatch for HeaderCaptureDispatch {
        async fn dispatch(
            &self,
            _client: &reqwest::Client,
            request: reqwest::Request,
        ) -> WireResult {
            *self.idempotency_key.lock().unwrap() = request
                .headers()
                .get(&IDEMPOTENCY_KEY_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            WireResult::RefusedBeforeWrite("synthetic refusal".into())
        }
    }

    #[async_trait]
    impl WireDispatch for InjectedDispatch {
        async fn dispatch(
            &self,
            _client: &reqwest::Client,
            _request: reqwest::Request,
        ) -> WireResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.lock().unwrap().take().unwrap()
        }
    }

    fn scope() -> ProviderRequestScope<'static> {
        ProviderRequestScope {
            credential_secret: b"synthetic-provider-secret",
            dialect: "openai_chat_completions",
            model: "synthetic-model",
            target_scope: "test-provider-send",
        }
    }

    fn request(client: &reqwest::Client) -> reqwest::RequestBuilder {
        client
            .post("http://127.0.0.1/provider")
            .bearer_auth("synthetic-provider-secret")
            .json(&serde_json::json!({"model": "synthetic-model"}))
    }

    fn reconciliation_authority() -> ProviderReconciliationAuthority {
        let custody =
            load_or_create_custody(&crate::discover::grokptah_home().join(CUSTODY_FILE)).unwrap();
        authenticate_provider_reconciliation(&custody).unwrap()
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn injected_prewrite_refusal_is_the_only_retryable_failure() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::RefusedBeforeWrite("refused".into()))),
        };
        let client = reqwest::Client::new();
        let error = send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
            .await
            .unwrap_err();
        assert!(error.is_safe_to_resend());
        assert!(!error.is_uncertain());
        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn host_binds_one_idempotency_key_and_rejects_caller_override() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = HeaderCaptureDispatch {
            idempotency_key: Mutex::new(None),
        };
        let client = reqwest::Client::new();
        let error = send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
            .await
            .unwrap_err();
        assert!(error.is_safe_to_resend());
        let key = dispatch.idempotency_key.lock().unwrap().clone().unwrap();
        assert!(key.starts_with("grokptah-att_"));

        let override_request = request(&client).header("Idempotency-Key", "caller-controlled");
        let error = send_provider_request_with(&client, override_request, scope(), None, &dispatch)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("must not supply or override"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn oauth_refresh_credential_is_exactly_once_in_the_form() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::RefusedBeforeWrite("refused".into()))),
        };
        let client = reqwest::Client::new();
        let oauth_scope = ProviderRequestScope {
            credential_secret: b"synthetic-refresh-token",
            dialect: "oauth2_refresh",
            model: "oidc-token-refresh",
            target_scope: "oidc-token-refresh",
        };
        let valid = client.post("http://127.0.0.1/token").form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "synthetic-refresh-token"),
        ]);
        assert!(
            send_provider_request_with(&client, valid, oauth_scope, None, &dispatch)
                .await
                .unwrap_err()
                .is_safe_to_resend()
        );
        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);

        let duplicate_location = client
            .post("http://127.0.0.1/token")
            .bearer_auth("synthetic-refresh-token")
            .form(&[("refresh_token", "synthetic-refresh-token")]);
        let error = send_provider_request_with(
            &client,
            duplicate_location,
            ProviderRequestScope {
                credential_secret: b"synthetic-refresh-token",
                dialect: "oauth2_refresh",
                model: "oidc-token-refresh",
                target_scope: "oidc-token-refresh",
            },
            None,
            &dispatch,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("must not also appear"));

        let duplicate_form = client
            .post("http://127.0.0.1/token")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body("refresh_token=synthetic-refresh-token&refresh_token=synthetic-refresh-token");
        let error = send_provider_request_with(
            &client,
            duplicate_form,
            ProviderRequestScope {
                credential_secret: b"synthetic-refresh-token",
                dialect: "oauth2_refresh",
                model: "oidc-token-refresh",
                target_scope: "oidc-token-refresh",
            },
            None,
            &dispatch,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("repeats its credential"));
        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn injected_postwrite_failure_is_uncertain_and_never_retryable() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Ambiguous("cut after write".into()))),
        };
        let client = reqwest::Client::new();
        let error = send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
            .await
            .unwrap_err();
        assert!(error.is_uncertain());
        assert!(!error.is_safe_to_resend());
        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn every_retry_oriented_http_status_is_uncertain_and_not_resendable() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let client = reqwest::Client::new();
        let operator = reconciliation_authority();

        for status in [408_u16, 429, 500, 502, 503, 504, 599] {
            let response: reqwest::Response = axum::http::Response::builder()
                .status(status)
                .body("synthetic retry-oriented response")
                .unwrap()
                .into();
            let dispatch = InjectedDispatch {
                calls: AtomicUsize::new(0),
                result: Mutex::new(Some(WireResult::Response(response))),
            };
            let response =
                send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
                    .await
                    .unwrap();
            assert!(is_retry_oriented_http_status(response.status()));
            let error = response.settle_http_failure(format!("HTTP {status}"));
            assert!(error.is_uncertain());
            assert!(!error.is_safe_to_resend());

            let attempts = provider_attempts_requiring_reconciliation(&operator).unwrap();
            assert_eq!(attempts.len(), 1);
            reconcile_provider_attempt(&operator, attempts[0], false).unwrap();
        }

        let response: reqwest::Response = axum::http::Response::builder()
            .status(400)
            .body("synthetic definitive refusal")
            .unwrap()
            .into();
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Response(response))),
        };
        let response =
            send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
                .await
                .unwrap();
        assert!(!is_retry_oriented_http_status(response.status()));
        let error = response.settle_http_failure("HTTP 400");
        assert!(!error.is_uncertain());
        assert!(!error.is_safe_to_resend());
        assert!(provider_attempts_requiring_reconciliation(&operator)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn cancellation_after_permit_is_uncertain_and_never_retryable() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let client = reqwest::Client::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = send_provider_request_with(
            &client,
            request(&client),
            scope(),
            Some(&cancel),
            &PendingDispatch,
        )
        .await
        .unwrap_err();
        assert!(error.is_uncertain());
        assert!(!error.is_safe_to_resend());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn response_abandoned_after_headers_requires_reconciliation() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let response: reqwest::Response = axum::http::Response::builder()
            .status(200)
            .body("synthetic body")
            .unwrap()
            .into();
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Response(response))),
        };
        let client = reqwest::Client::new();
        let response =
            send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
                .await
                .unwrap();
        drop(response);
        let operator = reconciliation_authority();
        let ambiguous = provider_attempts_requiring_reconciliation(&operator).unwrap();
        assert_eq!(ambiguous.len(), 1);
        reconcile_provider_attempt(&operator, ambiguous[0], true).unwrap();
        assert!(provider_attempts_requiring_reconciliation(&operator)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn unauthenticated_or_foreign_operator_cannot_reconcile_provider_attempts() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let response: reqwest::Response = axum::http::Response::builder()
            .status(200)
            .body("synthetic body")
            .unwrap()
            .into();
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Response(response))),
        };
        let client = reqwest::Client::new();
        let response =
            send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
                .await
                .unwrap();
        drop(response);
        assert!(authenticate_provider_reconciliation("wrong-custody-secret").is_err());
        let operator = reconciliation_authority();
        let attempts = provider_attempts_requiring_reconciliation(&operator).unwrap();
        assert_eq!(attempts.len(), 1);

        let foreign = tempfile::tempdir().unwrap();
        let foreign_operator = ProviderReconciliationAuthority {
            root: foreign.path().to_path_buf(),
        };
        assert!(provider_attempts_requiring_reconciliation(&foreign_operator).is_err());
        assert!(reconcile_provider_attempt(&foreign_operator, attempts[0], true).is_err());
        assert_eq!(
            provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn response_body_failure_and_protocol_rejection_are_uncertain() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let error_stream = futures::stream::once(async {
            Err::<Bytes, std::io::Error>(std::io::Error::other("cut after headers"))
        });
        let response: reqwest::Response = axum::http::Response::builder()
            .status(200)
            .body(reqwest::Body::wrap_stream(error_stream))
            .unwrap()
            .into();
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Response(response))),
        };
        let client = reqwest::Client::new();
        let mut response =
            send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
                .await
                .unwrap();
        assert!(response.next_chunk(None).await.unwrap_err().is_uncertain());
        let operator = reconciliation_authority();
        assert_eq!(
            provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len(),
            1
        );
        let first = provider_attempts_requiring_reconciliation(&operator).unwrap();
        reconcile_provider_attempt(&operator, first[0], false).unwrap();

        let response: reqwest::Response = axum::http::Response::builder()
            .status(200)
            .body("not-json")
            .unwrap()
            .into();
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Response(response))),
        };
        let mut response =
            send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
                .await
                .unwrap();
        let mut body = Vec::new();
        while let Some(chunk) = response.next_chunk(None).await.unwrap() {
            body.extend_from_slice(&chunk);
        }
        assert!(serde_json::from_slice::<serde_json::Value>(&body).is_err());
        let error = response.settle_protocol_error("malformed provider JSON");
        assert!(error.is_uncertain());
        assert_eq!(
            provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len(),
            1
        );
        let second = provider_attempts_requiring_reconciliation(&operator).unwrap();
        reconcile_provider_attempt(&operator, second[0], false).unwrap();

        let pending_stream = futures::stream::pending::<Result<Bytes, std::io::Error>>();
        let response: reqwest::Response = axum::http::Response::builder()
            .status(200)
            .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
            .body(reqwest::Body::wrap_stream(pending_stream))
            .unwrap()
            .into();
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::Response(response))),
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut response =
            send_provider_request_with(&client, request(&client), scope(), None, &dispatch)
                .await
                .unwrap();
        assert!(response
            .next_chunk(Some(&cancel))
            .await
            .unwrap_err()
            .is_uncertain());
        assert_eq!(
            provider_attempts_requiring_reconciliation(&operator)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mismatched_model_and_credential_are_refused_before_dispatch() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = InjectedDispatch {
            calls: AtomicUsize::new(0),
            result: Mutex::new(Some(WireResult::RefusedBeforeWrite("unused".into()))),
        };
        let client = reqwest::Client::new();
        let wrong_model = client
            .post("http://127.0.0.1/provider")
            .bearer_auth("synthetic-provider-secret")
            .json(&serde_json::json!({"model": "other-model"}));
        assert!(
            send_provider_request_with(&client, wrong_model, scope(), None, &dispatch,)
                .await
                .unwrap_err()
                .to_string()
                .contains("model scope")
        );
        let wrong_credential = client
            .post("http://127.0.0.1/provider")
            .bearer_auth("other-secret")
            .json(&serde_json::json!({"model": "synthetic-model"}));
        assert!(
            send_provider_request_with(&client, wrong_credential, scope(), None, &dispatch,)
                .await
                .unwrap_err()
                .to_string()
                .contains("credential scope")
        );
        let duplicate_header = client
            .post("http://127.0.0.1/provider")
            .header(AUTHORIZATION, "Bearer synthetic-provider-secret")
            .header(AUTHORIZATION, "Bearer synthetic-provider-secret")
            .json(&serde_json::json!({"model": "synthetic-model"}));
        assert!(
            send_provider_request_with(&client, duplicate_header, scope(), None, &dispatch,)
                .await
                .unwrap_err()
                .to_string()
                .contains("repeats its Authorization")
        );
        let duplicated_in_body = client
            .post("http://127.0.0.1/provider")
            .bearer_auth("synthetic-provider-secret")
            .json(&serde_json::json!({
                "model": "synthetic-model",
                "prompt": "synthetic-provider-secret"
            }));
        assert!(
            send_provider_request_with(&client, duplicated_in_body, scope(), None, &dispatch,)
                .await
                .unwrap_err()
                .to_string()
                .contains("more than one wire location")
        );
        let streamed_body = reqwest::Body::wrap_stream(futures::stream::once(async {
            Ok::<Bytes, std::io::Error>(Bytes::from_static(br#"{"model":"synthetic-model"}"#))
        }));
        let unsupported_stream = client
            .post("http://127.0.0.1/provider")
            .bearer_auth("synthetic-provider-secret")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(streamed_body);
        assert!(
            send_provider_request_with(&client, unsupported_stream, scope(), None, &dispatch,)
                .await
                .unwrap_err()
                .to_string()
                .contains("immutable bytes")
        );
        assert_eq!(dispatch.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn oauth_refresh_does_not_bind_a_wire_idempotency_key() {
        let home = tempfile::tempdir().unwrap();
        let _serial = crate::discover::home_override_serial();
        let _home = HomeOverride::install(home.path().to_path_buf());
        let dispatch = HeaderCaptureDispatch {
            idempotency_key: Mutex::new(None),
        };
        let client = reqwest::Client::new();
        let oauth_scope = ProviderRequestScope {
            credential_secret: b"synthetic-refresh-token",
            dialect: "oauth2_refresh",
            model: "oidc-token-refresh",
            target_scope: "oidc-token-refresh",
        };
        let valid = client.post("http://127.0.0.1/token").form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "synthetic-refresh-token"),
        ]);
        let _ = send_provider_request_with(&client, valid, oauth_scope, None, &dispatch)
            .await
            .unwrap_err();
        assert!(dispatch.idempotency_key.lock().unwrap().is_none());
        assert!(!dialect_supports_wire_idempotency("oauth2_refresh"));
    }

    #[test]
    fn credential_bearing_model_calls_have_no_raw_send_escape_hatch() {
        let raw_send = [".", "send()"].concat();
        let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in walkdir::WalkDir::new(&source_root) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("rs")
            {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap();
            let name = relative.to_string_lossy();
            if name.ends_with("provider_transport.rs")
                || name.ends_with("mcp_control.rs")
                || name.ends_with("mcp_control_client.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(entry.path()).unwrap();
            for (offset, _) in source.match_indices(&raw_send) {
                let context = source[..offset]
                    .lines()
                    .rev()
                    .take(10)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    context.contains("authority-allow-unauthenticated-wire"),
                    "raw reqwest send outside provider authority in {name}"
                );
            }
        }
        let this_module = include_str!("provider_transport.rs");
        let execute_needle = ["client.execute", "(request)"].concat();
        assert_eq!(this_module.matches(&execute_needle).count(), 1);
        assert!(this_module.contains("admit_sending"));

        let public_surface = include_str!("lib.rs");
        assert!(public_surface.contains(
            "authority: &ProviderReconciliationAuthority,\n    attempt: ProviderAttemptId,"
        ));
        assert!(public_surface.contains(
            "provider_attempts_requiring_reconciliation(\n    authority: &ProviderReconciliationAuthority,"
        ));
        assert!(!public_surface
            .contains("reconcile_provider_attempt(\n    attempt: ProviderAttemptId,"));
    }
}
